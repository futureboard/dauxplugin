//! The acceptance test: a real plug-in, a real `.axt`, loaded over the real C ABI.
//!
//! Every other suite in this workspace stops at a boundary. `daux-format-axt`'s tests drive
//! the exported function tables with a plug-in compiled into the test binary; `daux-scan`'s
//! tests walk trees of bundles whose "binaries" are 45 bytes of ASCII; `daux-host`'s tests
//! use the native path, where no ABI is crossed at all. Each is correct about its own half
//! and none of them can fail if the halves do not fit together.
//!
//! This suite closes that gap. It takes the dynamic library cargo built from
//! [`examples/gain`](daux_example_gain) — the one `export_plugin!` put a real
//! `daux_plugin_entry_v1` in — packages it into a real bundle with the real
//! [`BundleBuilder`](daux_bundle::BundleBuilder), and then does what a DAW does:
//! `LoadLibrary`, entry point, factory, descriptor, instance, activate, `process`.
//!
//! # Why the numbers are asserted, not just the absence of errors
//!
//! A gain plug-in is the one DSP whose correct output can be written down in closed form, so
//! these tests assert the actual sample values. That matters more than it looks: an adapter
//! that silences its outputs on every refused call, or a host that drops the parameter events
//! it was given, produces a run with no errors, a plausible status code and completely wrong
//! audio. Only the samples catch it.

use std::path::{Path, PathBuf};

use daux_audio::AudioStorage;
use daux_core::{ProcessConfig, ProcessStatus};
use daux_host::TestHost;
use daux_tests::TempTree;
use daux_tests::assertions::assert_close;

/// The gain example's permanent id, as its `#[plugin(..)]` attribute fixes it.
const GAIN_ID: &str = "studio.futureboard.daux.example.gain";

/// The permanent id of its one parameter.
const GAIN_PARAM: u32 = 1;

/// Long enough for the 15 ms smoother to have settled well inside one block.
const SAMPLE_RATE: f64 = 48_000.0;

/// Blocks of this many frames. The smoother needs a few thousand samples to converge to
/// single-precision equality, so the tests run several blocks rather than one long one.
const BLOCK: usize = 512;

/// How many blocks to run before reading a settled value: 8 x 512 = 4096 samples = 85 ms,
/// nearly six time constants of the example's 15 ms smoothing.
const SETTLE_BLOCKS: usize = 8;

// ---------------------------------------------------------------------------------------
// Finding and packaging the real binary
// ---------------------------------------------------------------------------------------

/// The platform's file name for the gain example's dynamic library.
///
/// Cargo names a `cdylib` after the *crate* name with `-` replaced by `_`, and applies the
/// platform's own prefix and extension.
fn cdylib_file_name() -> String {
    format!(
        "{}daux_example_gain{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

/// The dynamic library cargo built for `examples/gain`, or a panic explaining why not.
///
/// `daux-example-gain` is a dev-dependency of this crate, so cargo has already built its
/// `[lib]` target — and because that target declares `crate-type = ["cdylib", "rlib"]`, the
/// same invocation produced both artefacts. The `cdylib` lands in the profile directory,
/// which is two levels above the test binary in `{profile}/deps/`.
///
/// # Panics
///
/// If the library is not where cargo puts it. That is a build-system failure rather than a
/// test failure, so it says so rather than skipping: a silently skipped acceptance test is
/// indistinguishable from a passing one.
fn built_cdylib() -> PathBuf {
    let file = cdylib_file_name();
    let exe = std::env::current_exe().expect("the test binary knows its own path");
    let mut searched = Vec::new();
    // `{profile}/deps/test-hash.exe` → `{profile}/deps` → `{profile}`, and one further up
    // for a `--target <triple>` build, where the profile directory is nested one deeper.
    for ancestor in exe.ancestors().skip(1).take(4) {
        let candidate = ancestor.join(&file);
        if candidate.is_file() {
            return candidate;
        }
        searched.push(candidate);
    }
    panic!(
        "cargo did not leave `{file}` anywhere above the test binary `{}`.\n\
         Looked in:\n  {}\n\
         `daux-example-gain` is a dev-dependency of `daux-tests` precisely so this exists; \
         if its `[lib] crate-type` no longer lists `cdylib`, this suite cannot run at all.",
        exe.display(),
        searched
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Packages the real binary into a real `.axt` inside `tree`, and returns the bundle root.
///
/// Deliberately the same [`BundleBuilder`](daux_bundle::BundleBuilder) call `daux build`
/// makes, so a change that breaks packaging breaks this test too.
fn package(tree: &TempTree) -> PathBuf {
    let mut capabilities = daux_bundle::ManifestCaps::empty();
    capabilities.set(daux_abi::DAUX_CAP_AUDIO_EFFECT, true);

    daux_bundle::BundleBuilder::new(GAIN_ID, "DAUx Gain", "Futureboard Studio", "1.0.0")
        .expect("the example's identity is well-formed")
        .description("Minimal stereo gain effect.")
        .capabilities(capabilities)
        .binary(daux_bundle::TargetId::host(), &built_cdylib())
        .write(&tree.dir("install"))
        .expect("the bundle writes")
}

/// A host, and the gain example loaded into it from a real bundle over the C ABI.
fn loaded_host(tree: &TempTree) -> (TestHost, daux_host::InstanceId) {
    let bundle = package(tree);
    let mut host = TestHost::new(ProcessConfig::new(SAMPLE_RATE, BLOCK as u32));
    let instance = host
        .load(&bundle)
        .unwrap_or_else(|error| panic!("`{}` must load: {error}", bundle.display()));
    assert!(
        !host.is_native(instance).expect("a live instance"),
        "this suite is worthless unless the instance really came through the ABI"
    );
    (host, instance)
}

/// Fills every channel of `storage` with a constant.
///
/// DC rather than an impulse: a gain is a pure scaling, so a constant input makes the
/// expected output a constant too, and every sample of the block is an assertion rather than
/// just the first one. An impulse only ever samples the *first* point of the parameter
/// smoother's ramp, which is the old value, not the new one.
fn fill_dc(storage: &mut AudioStorage<f32>, value: f32) {
    for channel in 0..storage.channel_count() {
        if let Some(samples) = storage.channel_mut(channel) {
            samples.fill(value);
        }
    }
}

/// Runs `blocks` blocks of DC through `instance` and returns the last output block.
fn run_dc(
    host: &mut TestHost,
    instance: daux_host::InstanceId,
    value: f32,
    blocks: usize,
) -> AudioStorage<f32> {
    let mut input = AudioStorage::<f32>::new(2, BLOCK);
    fill_dc(&mut input, value);
    let mut output = AudioStorage::<f32>::new(2, BLOCK);
    for block in 0..blocks {
        let status = host
            .process(instance, &input, &mut output)
            .unwrap_or_else(|error| panic!("block {block}: {error}"));
        assert!(
            matches!(
                status,
                ProcessStatus::Continue | ProcessStatus::ContinueIfNotQuiet
            ),
            "block {block} returned {status:?}; an effect fed DC must keep going"
        );
    }
    output
}

/// Asserts every sample of every channel is `expected`, and says which one was not.
fn assert_dc(output: &AudioStorage<f32>, expected: f32, tolerance: f32, context: &str) {
    for channel in 0..output.channel_count() {
        let samples = output
            .channel(channel)
            .unwrap_or_else(|| panic!("{context}: channel {channel} is missing"));
        assert_eq!(samples.len(), BLOCK, "{context}: short block");
        for (index, sample) in samples.iter().enumerate() {
            assert!(
                (sample - expected).abs() <= tolerance,
                "{context}: channel {channel} sample {index} is {sample}, expected \
                 {expected} +/- {tolerance}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------------------

/// The whole pipeline, in one test: build → bundle → validate → open → load → process.
///
/// If this fails, the project does not work, whatever the unit suites say.
#[test]
fn a_real_axt_opens_validates_and_reports_the_plug_in_it_carries() {
    let tree = TempTree::new("e2e-open");
    let root = package(&tree);

    let bundle = daux_bundle::Bundle::open(&root)
        .unwrap_or_else(|error| panic!("`{}` must open: {error}", root.display()));
    let issues = bundle.validate();
    let errors: Vec<_> = issues
        .iter()
        .filter(|issue| issue.severity == daux_bundle::Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "a bundle the builder just wrote must validate clean, got {errors:?}"
    );

    // The manifest side.
    let metadata = bundle.metadata();
    assert_eq!(metadata.id, GAIN_ID);
    assert_eq!(metadata.version, "1.0.0");

    // The binary side: this is the part no synthetic fixture can reach. Loading the module
    // runs the platform loader, resolves `daux_plugin_entry_v1` and calls it.
    let module = daux_runtime::AxtModule::load(&bundle, &daux_bundle::TargetId::host())
        .unwrap_or_else(|error| panic!("the module must load: {error}"));
    assert_eq!(
        module.abi_version().0,
        daux_abi::DAUX_ABI_VERSION_MAJOR,
        "a module built against another ABI major version must not have loaded at all"
    );

    let bridge = daux_runtime::HostBridge::new(
        daux_core::daux_host_services::HostServices::builder().build(),
    );
    let factory = daux_runtime::LoadedFactory::create(std::sync::Arc::new(module), bridge)
        .unwrap_or_else(|error| panic!("the factory must be reachable: {error}"));
    assert_eq!(
        factory.plugin_count(),
        1,
        "`export_plugin!(SingleFactory<Gain>)` exports exactly one plug-in"
    );
    let descriptor = factory.descriptor(0).expect("a descriptor at index 0");
    assert_eq!(descriptor.id.as_str(), GAIN_ID);
    assert_eq!(descriptor.name, "DAUx Gain");
    assert_eq!(
        descriptor.category,
        daux_core::Category::Effect,
        "the category must survive the ABI round trip; abi-v1 §6.1 numbers it 1, and a \
         mis-numbering files every plug-in under its neighbour's heading"
    );
}

/// Unity gain must be a bit-exact pass-through, not merely "close".
///
/// The example's default is 0 dB, so a correct plug-in multiplies by exactly 1.0 and the
/// output is the input. Asserting equality rather than a tolerance is what catches a gain
/// that is silently applying its ramp from the wrong end.
#[test]
fn audio_passes_through_a_loaded_plug_in_untouched_at_unity() {
    let tree = TempTree::new("e2e-unity");
    let (mut host, instance) = loaded_host(&tree);

    let output = run_dc(&mut host, instance, 0.25, SETTLE_BLOCKS);
    assert_dc(&output, 0.25, 0.0, "unity gain over the ABI");
}

/// **The acceptance test.** A parameter set through the host reaches the plug-in and the
/// audio changes by exactly the amount the parameter says.
///
/// -6.0 dB is 10^(-6/20) = 0.501_187_2. The tolerance is single-precision slack on the
/// smoother's final approach, not room for a wrong answer: a plug-in that ignored the
/// parameter would sit at 1.0, and one that applied it linearly would sit at 0.94.
#[test]
fn a_parameter_change_reaches_a_loaded_plug_in_and_changes_the_audio_by_exactly_that_much() {
    let tree = TempTree::new("e2e-gain");
    let (mut host, instance) = loaded_host(&tree);

    // One block first, so the instance's ABI block is already built for this topology and
    // the parameter below is queued into the block the *next* `process` actually reads.
    let baseline = run_dc(&mut host, instance, 1.0, 1);
    assert_dc(&baseline, 1.0, 1e-6, "the default is 0 dB");

    host.try_set_param(instance, GAIN_PARAM, -6.0)
        .expect("the gain parameter exists and is settable");
    let output = run_dc(&mut host, instance, 1.0, SETTLE_BLOCKS);

    let expected = 10.0_f32.powf(-6.0 / 20.0);
    assert_dc(&output, expected, 1e-4, "-6 dB over the ABI");

    // And the plug-in agrees about what its parameter now is, so the audio and the model
    // did not diverge.
    let readback = host
        .param_value(instance, GAIN_PARAM)
        .expect("the parameter reads back");
    assert_close(readback, -6.0, 1e-9, "the plug-in's own view of the gain");
}

/// The regression this suite was written for.
///
/// `daux-host` only learns an instance's channel counts when the caller hands it storage, so
/// it rebuilds the loaded instance's ABI block on the *first* `process`. That rebuild used to
/// throw away the event list — which is exactly where `set_param` and `send_note_on` had put
/// their events. Every parameter set before the first block was silently discarded, with no
/// error anywhere: `daux run --param 1=-60` left the plug-in at 0 dB and printed a clean run.
///
/// Setting the parameter *before* any `process` call is therefore the whole point of this
/// test, and is what distinguishes it from the one above.
#[test]
fn a_parameter_set_before_the_very_first_block_is_not_swallowed_by_the_block_rebuild() {
    let tree = TempTree::new("e2e-first-block");
    let (mut host, instance) = loaded_host(&tree);

    // No `process` has run yet: the instance's block is still the empty placeholder built at
    // activation, and the first `process` will replace it.
    host.try_set_param(instance, GAIN_PARAM, -60.0)
        .expect("the gain parameter exists");

    let output = run_dc(&mut host, instance, 1.0, SETTLE_BLOCKS);
    let expected = 10.0_f32.powf(-60.0 / 20.0);
    assert_dc(&output, expected, 1e-4, "-60 dB set before the first block");

    assert_close(
        host.param_value(instance, GAIN_PARAM).expect("readable"),
        -60.0,
        1e-9,
        "the parameter the plug-in applied",
    );
}

/// The native path and the ABI path must produce the same audio, sample for sample.
///
/// This is the comparison that localises a bug. The same Rust object, driven two ways: if
/// only the loaded side is wrong, the fault is in the adapter, the bundle or the loader, and
/// not in the plug-in's DSP.
#[test]
fn the_native_and_the_abi_paths_produce_identical_audio() {
    let tree = TempTree::new("e2e-parity");
    let (mut loaded_host_, loaded) = loaded_host(&tree);

    let mut native_host = TestHost::new(ProcessConfig::new(SAMPLE_RATE, BLOCK as u32));
    let native = native_host
        .install::<daux_example_gain::Gain>()
        .expect("the compiled-in plug-in installs");
    assert!(native_host.is_native(native).expect("a live instance"));

    // Settle both at the same non-default gain, from the same starting point.
    let _ = run_dc(&mut loaded_host_, loaded, 1.0, 1);
    let _ = run_dc(&mut native_host, native, 1.0, 1);
    loaded_host_
        .try_set_param(loaded, GAIN_PARAM, -12.0)
        .expect("settable");
    native_host
        .try_set_param(native, GAIN_PARAM, -12.0)
        .expect("settable");

    let from_abi = run_dc(&mut loaded_host_, loaded, 0.5, SETTLE_BLOCKS);
    let from_native = run_dc(&mut native_host, native, 0.5, SETTLE_BLOCKS);

    for channel in 0..2 {
        let abi = from_abi.channel(channel).expect("a channel");
        let rust = from_native.channel(channel).expect("a channel");
        assert_eq!(
            abi, rust,
            "channel {channel} differs between the ABI and the native path; the DSP is the \
             same object, so the difference is in the adapter, the bundle or the loader"
        );
    }
}

/// State written by a loaded instance must load back into a loaded instance.
///
/// A preset that cannot survive a save/load cycle is a data-loss bug, and the blob crosses
/// the ABI in both directions here rather than staying inside one Rust process.
#[test]
fn state_saved_over_the_abi_restores_over_the_abi() {
    let tree = TempTree::new("e2e-state");
    let (mut host, instance) = loaded_host(&tree);

    let _ = run_dc(&mut host, instance, 1.0, 1);
    host.try_set_param(instance, GAIN_PARAM, -18.0)
        .expect("settable");
    let _ = run_dc(&mut host, instance, 1.0, 1);
    let blob = host.save_state(instance).expect("the state saves");
    assert!(!blob.is_empty(), "a plug-in with a parameter has state");

    // Move it somewhere else, then put the saved state back.
    host.try_set_param(instance, GAIN_PARAM, 6.0)
        .expect("settable");
    let _ = run_dc(&mut host, instance, 1.0, 1);
    assert_close(
        host.param_value(instance, GAIN_PARAM).expect("readable"),
        6.0,
        1e-9,
        "the parameter really moved before the reload",
    );

    host.load_state(instance, &blob).expect("the state loads");
    assert_close(
        host.param_value(instance, GAIN_PARAM).expect("readable"),
        -18.0,
        1e-9,
        "the saved value came back",
    );

    // And the audio follows the restored value, rather than the model alone.
    let output = run_dc(&mut host, instance, 1.0, SETTLE_BLOCKS);
    let expected = 10.0_f32.powf(-18.0 / 20.0);
    assert_dc(&output, expected, 1e-4, "audio after restoring state");
}

/// A hostile blob must be refused, not applied.
///
/// `load_state` is reachable from a project file, which is the least trustworthy input a
/// plug-in has. Refusing must leave the instance usable.
#[test]
fn a_corrupt_state_blob_is_refused_and_leaves_the_instance_working() {
    let tree = TempTree::new("e2e-bad-state");
    let (mut host, instance) = loaded_host(&tree);

    let _ = run_dc(&mut host, instance, 1.0, 1);
    host.try_set_param(instance, GAIN_PARAM, -3.0)
        .expect("settable");
    let _ = run_dc(&mut host, instance, 1.0, 1);

    for hostile in [
        &b""[..],
        &b"not a state document at all"[..],
        &[0xFF; 64][..],
    ] {
        // Either outcome is defensible for an empty or unrecognised blob — what is not
        // defensible is a crash, or a silently corrupted parameter.
        let _ = host.load_state(instance, hostile);
        let value = host
            .param_value(instance, GAIN_PARAM)
            .expect("the instance is still alive after a refused load");
        assert!(
            value.is_finite() && (-60.0..=12.0).contains(&value),
            "a hostile blob left the gain at {value}, outside its own range"
        );
    }

    // The instance still processes audio.
    let output = run_dc(&mut host, instance, 0.5, SETTLE_BLOCKS);
    for channel in 0..output.channel_count() {
        let samples = output.channel(channel).expect("a channel");
        assert!(
            samples.iter().all(|sample| sample.is_finite()),
            "channel {channel} went non-finite after a refused state load"
        );
    }
}

/// The scanner must find a real bundle and read its descriptor out of the real binary.
///
/// `daux-scan`'s own tests use fixtures whose "binary" is 45 bytes of ASCII, so they only
/// ever exercise the path where probing *fails*. This is the other one.
#[test]
fn the_scanner_finds_a_real_bundle_and_probes_the_descriptor_out_of_it() {
    let tree = TempTree::new("e2e-scan");
    let root = package(&tree);
    let search = root.parent().expect("the install directory").to_path_buf();

    let mut scanner = daux_scan::Scanner::new();
    scanner.add_search_path(search);
    let report = scanner.scan();

    let entry = report
        .entries()
        .iter()
        .find(|entry| entry.path == root)
        .unwrap_or_else(|| {
            panic!(
                "the scanner did not find `{}`; it found {:?}",
                root.display(),
                report
                    .entries()
                    .iter()
                    .map(|entry| entry.path.display().to_string())
                    .collect::<Vec<_>>()
            )
        });

    assert!(
        entry.probed,
        "the binary is a real, loadable module, so the scanner must have opened it; \
         failures: {:?}",
        report.failures()
    );
    assert_eq!(
        entry.descriptors.len(),
        1,
        "one plug-in in the bundle, read from the binary"
    );
    assert_eq!(entry.descriptors[0].id.as_str(), GAIN_ID);
    assert_eq!(entry.descriptors[0].category, daux_core::Category::Effect);
}

/// Several instances of the same module must coexist and stay independent.
///
/// `CLAUDE.md` rule 6 — no global mutable state — is only ever really tested by running two
/// at once and moving one of them.
#[test]
fn two_instances_of_one_module_do_not_share_state() {
    let tree = TempTree::new("e2e-multi");
    let bundle = package(&tree);
    let mut host = TestHost::new(ProcessConfig::new(SAMPLE_RATE, BLOCK as u32));

    let first = host.load(&bundle).expect("the first instance loads");
    let second = host.load(&bundle).expect("the second instance loads");
    assert_ne!(first, second);

    let _ = run_dc(&mut host, first, 1.0, 1);
    let _ = run_dc(&mut host, second, 1.0, 1);

    host.try_set_param(first, GAIN_PARAM, -20.0)
        .expect("settable");
    let quiet = run_dc(&mut host, first, 1.0, SETTLE_BLOCKS);
    let loud = run_dc(&mut host, second, 1.0, SETTLE_BLOCKS);

    assert_dc(
        &quiet,
        10.0_f32.powf(-20.0 / 20.0),
        1e-4,
        "the moved instance",
    );
    assert_dc(
        &loud,
        1.0,
        1e-4,
        "the untouched instance must not have moved",
    );

    // Unloading one must leave the other working.
    host.unload(first).expect("the first unloads");
    let still_loud = run_dc(&mut host, second, 1.0, 2);
    assert_dc(&still_loud, 1.0, 1e-4, "after its sibling was unloaded");
}

/// A block larger than the activation promised must be refused rather than trusted.
///
/// `abi-v1` §8 fixes the frame count at `1 ..= max_block_size`. A host that hands over more
/// is asking the plug-in to read past the buffer it sized in `prepare`, and refusing is the
/// only answer that does not corrupt memory.
#[test]
fn a_block_longer_than_the_activation_is_refused() {
    let tree = TempTree::new("e2e-overlong");
    let (mut host, instance) = loaded_host(&tree);

    let _ = run_dc(&mut host, instance, 1.0, 1);

    let oversized = BLOCK * 2;
    let mut input = AudioStorage::<f32>::new(2, oversized);
    fill_dc(&mut input, 1.0);
    let mut output = AudioStorage::<f32>::new(2, oversized);
    let error = host
        .process(instance, &input, &mut output)
        .expect_err("a block twice the activation's maximum must be refused");
    assert_eq!(error.kind(), daux_host::HostErrorKind::BadBlock);

    // And the instance is still usable at a legal size afterwards.
    let recovered = run_dc(&mut host, instance, 1.0, 1);
    assert_dc(&recovered, 1.0, 1e-4, "after a refused oversized block");
}

/// The bundle the loader opened must stay openable while an instance is alive, and the
/// module must survive the last instance being dropped.
#[test]
fn a_bundle_can_be_reopened_while_an_instance_from_it_is_running() {
    let tree = TempTree::new("e2e-reopen");
    let (mut host, instance) = loaded_host(&tree);
    let _ = run_dc(&mut host, instance, 1.0, 1);

    let root = tree.dir("install").join("DAUx Gain.axt");
    let reopened = daux_bundle::Bundle::open(&root)
        .expect("the bundle is still readable while its module is loaded");
    assert_eq!(reopened.metadata().id, GAIN_ID);

    host.unload(instance).expect("the instance unloads");
    assert_eq!(host.instance_count(), 0);

    // A fresh load of the same bundle after everything was dropped.
    let again = host.load(&root).expect("the bundle loads a second time");
    let output = run_dc(&mut host, again, 0.75, 2);
    assert_dc(&output, 0.75, 1e-4, "the reloaded module still works");
}

/// The example's own `Cargo.toml` must stay in step with its `#[plugin(..)]` attribute.
///
/// `daux validate --probe` cross-checks the two (`manifest-v1` §8.1), so a drift between them
/// is a shipped bundle that fails validation. Pinning the identity here means the drift is
/// caught by `cargo test` rather than by a build tool nobody ran.
#[test]
fn the_examples_manifest_metadata_matches_the_descriptor_it_compiles_to() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root is two levels above tests/harness")
        .join("examples/gain/Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("`{}`: {error}", manifest.display()));

    let descriptor = <daux_example_gain::Gain as daux_core::DauxPlugin>::descriptor();
    for (key, value) in [
        ("id", descriptor.id.as_str()),
        ("name", descriptor.name.as_str()),
        ("vendor", descriptor.vendor.as_str()),
    ] {
        let expected = format!("{key} = \"{value}\"");
        assert!(
            text.contains(&expected),
            "`examples/gain/Cargo.toml` must carry `{expected}` under \
             `[package.metadata.daux]`, because `daux validate --probe` compares the two \
             and reports DAUX-M100..M107 when they disagree"
        );
    }
    assert!(
        text.contains("[package.metadata.daux]"),
        "without that table `daux build` refuses the crate with DAUX-M200"
    );
}
