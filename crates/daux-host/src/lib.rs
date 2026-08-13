//! An in-process host, small enough to put in a unit test.
//!
//! A plug-in is normally only testable inside a DAW, which is the worst place to find a bug:
//! the feedback loop is minutes long, the failure is a noise rather than an assertion, and
//! the interesting cases — a host that refuses to resize the editor, a full worker queue, a
//! block of one frame, a state blob from last year — never happen by accident. This crate
//! turns all of them into ordinary `cargo test` code.
//!
//! ```
//! use daux_host::TestHost;
//! use daux_host::daux_core::ProcessConfig;
//!
//! let mut host = TestHost::new(ProcessConfig::new(48_000.0, 512));
//! assert_eq!(host.instance_count(), 0);
//! // `host.install::<MyPlugin>()?` for a plug-in compiled in, or
//! // `host.load(Path::new("Gain.axt"))?` for one on disk.
//! ```
//!
//! # Two ways in, one way to drive
//!
//! | | [`TestHost::install`] | [`TestHost::load`] |
//! | --- | --- | --- |
//! | Takes | a `DauxPlugin` compiled into the test | an `.axt` directory |
//! | Goes through | the Rust object model | the C ABI, `dlopen` and all |
//! | Needs a build step | no | yes |
//!
//! Every other call — [`set_param`](TestHost::set_param),
//! [`send_note_on`](TestHost::send_note_on), [`process`](TestHost::process),
//! [`save_state`](TestHost::save_state), [`load_state`](TestHost::load_state) — works the
//! same on both. A test that passes one way and fails the other has found a bug in the
//! format adapter rather than in the plug-in, which is the comparison that makes the second
//! path worth having at all.
//!
//! # The host is real
//!
//! Every `daux.host.*` service is implemented rather than stubbed, and [`HarnessHost`]
//! records what was asked of it: logging into a bounded lock-free queue, gestures and
//! parameter changes in order, worker requests that can be made to fail, editor and timer
//! requests that can be refused, and resources resolved through the bundle's own confinement
//! rules. A plug-in tested against a host that always says yes is a plug-in that has never
//! been tested.
//!
//! # Threading
//!
//! Everything here is `[main-thread]` except [`TestHost::process`], which is the call a real
//! host makes from its audio thread. The harness does not spawn an audio thread of its own:
//! a test that wants one moves the whole [`TestHost`] there, which is exactly the ownership
//! a DAW has.
//!
//! # `unsafe`
//!
//! Two blocks, both in `instance.rs`, both binding a sample pointer into a
//! [`HostBlock`](daux_runtime::HostBlock) for one `process` call over the C ABI — the ABI
//! hands a plug-in `*mut f32` for its inputs as well as its outputs (`abi-v1` §8), so there
//! is no safe way to describe a block. Every binding is dropped again before the call
//! returns. The native path contains no `unsafe` at all.

mod error;
mod host;
mod instance;
mod services;

#[cfg(test)]
mod testplugin;

pub use error::{HostError, HostErrorKind, HostResult};
pub use host::TestHost;
pub use instance::InstanceId;
pub use services::{GuiRequest, HarnessHost, LogRecord, ParamActivity};

/// The crates whose types appear in this one's signatures, re-exported so a test can name
/// them without adding each dependency itself.
pub use {daux_audio, daux_events, daux_midi, daux_parameter, daux_runtime, daux_transport};

/// The plug-in object model, re-exported for the same reason.
pub use daux_runtime::daux_core;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testplugin::{BYPASS_ID, GAIN_ID, GainPlugin, Probe, RefusingPlugin};
    use daux_audio::AudioStorage;
    use daux_runtime::daux_core::{DauxPlugin, Latency, ProcessConfig, ProcessStatus, Tail};
    use daux_runtime::daux_host_services::{HostLog, HostWorker, TaskId};
    use std::sync::Arc;

    fn host() -> TestHost {
        TestHost::new(ProcessConfig::new(48_000.0, 512))
    }

    /// Installs the fixture and keeps a handle on what it is asked to do.
    fn host_with_probe() -> (TestHost, InstanceId, Arc<Probe>) {
        let mut host = host();
        let probe = Arc::new(Probe::default());
        let instance = host
            .install_plugin(
                Box::new(GainPlugin::with_probe(Arc::clone(&probe))),
                GainPlugin::descriptor(),
            )
            .expect("installs");
        (host, instance, probe)
    }

    fn ramp(channels: usize, frames: usize) -> AudioStorage<f32> {
        let mut storage = AudioStorage::<f32>::new(channels, frames);
        for channel in 0..channels {
            let samples = storage.channel_mut(channel).expect("a channel");
            for (index, sample) in samples.iter_mut().enumerate() {
                *sample = index as f32 + 1.0;
            }
        }
        storage
    }

    #[test]
    fn an_installed_plug_in_processes_audio() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");
        assert_eq!(host.instance_count(), 1);
        assert!(host.is_native(gain).expect("known"));
        assert_eq!(
            host.descriptor(gain).expect("known").id.as_str(),
            "com.futureboard.test.gain"
        );

        let input = ramp(2, 8);
        let mut output = AudioStorage::<f32>::new(2, 8);
        let status = host.process(gain, &input, &mut output).expect("a block");
        assert_eq!(status, ProcessStatus::Continue);
        // Unity gain by default, so the ramp comes through untouched.
        assert_eq!(
            output.channel(0).expect("a channel"),
            input.channel(0).unwrap()
        );
    }

    /// The parameter path end to end: the host writes a plain value, the plug-in applies it
    /// to the samples, and the host can read it back.
    #[test]
    fn a_parameter_change_reaches_the_audio() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");

        host.set_param(gain, GAIN_ID.0, 2.0);
        assert_eq!(host.param_value(gain, GAIN_ID.0).expect("known"), 2.0);

        let input = ramp(1, 4);
        let mut output = AudioStorage::<f32>::new(1, 4);
        host.process(gain, &input, &mut output).expect("a block");
        assert_eq!(output.channel(0).unwrap(), &[2.0, 4.0, 6.0, 8.0]);

        // And a second block keeps the value: automation is not re-sent every block.
        let mut again = AudioStorage::<f32>::new(1, 4);
        host.process(gain, &input, &mut again).expect("a block");
        assert_eq!(again.channel(0).unwrap(), &[2.0, 4.0, 6.0, 8.0]);
    }

    /// A parameter the plug-in does not have must be reported, not silently swallowed —
    /// otherwise a renumbered id looks exactly like a working one.
    #[test]
    fn an_unknown_parameter_is_reported_and_recorded() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");

        let error = host
            .try_set_param(gain, 9_999, 1.0)
            .expect_err("there is no parameter 9999");
        assert_eq!(error.kind(), HostErrorKind::NoSuchParam);
        assert!(host.param_value(gain, 9_999).is_err());

        // The infallible form still leaves a trace, so a test that used it can find out.
        host.set_param(gain, 9_999, 1.0);
        let log = host.host().drain_log();
        assert!(
            log.iter()
                .any(|record| record.message.contains("set_param")),
            "{log:?}"
        );
    }

    #[test]
    fn notes_reach_the_plug_in_and_its_answer_reaches_the_host() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");

        host.send_note_on(gain, 0, 60, 0.8);
        host.send_note_on(gain, 3, 64, 0.9);
        let input = ramp(1, 8);
        let mut output = AudioStorage::<f32>::new(1, 8);
        host.process(gain, &input, &mut output).expect("a block");

        assert_eq!(
            host.output_event_count(gain).expect("known"),
            2,
            "the plug-in echoes each note back"
        );
        let events = host.output_events(gain).expect("a native instance");
        assert_eq!(events.len(), 2);

        // The queue is drained by the block: a note must not sound twice.
        let mut again = AudioStorage::<f32>::new(1, 8);
        host.process(gain, &input, &mut again).expect("a block");
        assert_eq!(host.output_event_count(gain).expect("known"), 0);
    }

    /// Every one of these is a block a plug-in would read or write past the end of.
    #[test]
    fn a_block_that_cannot_be_described_is_refused_before_the_plug_in_sees_it() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");
        let input = ramp(1, 8);

        let mut empty = AudioStorage::<f32>::new(1, 0);
        assert_eq!(
            host.process(gain, &input, &mut empty).unwrap_err().kind(),
            HostErrorKind::BadBlock,
            "a block of no frames is not a block"
        );

        let mut channelless = AudioStorage::<f32>::new(0, 8);
        assert_eq!(
            host.process(gain, &input, &mut channelless)
                .unwrap_err()
                .kind(),
            HostErrorKind::BadBlock
        );

        // `abi-v1` §8: never more frames than the activation allows.
        let long_input = ramp(1, 1_024);
        let mut long = AudioStorage::<f32>::new(1, 1_024);
        let error = host.process(gain, &long_input, &mut long).unwrap_err();
        assert_eq!(error.kind(), HostErrorKind::BadBlock);
        assert!(error.message().contains("512"), "{error}");

        // An input and an output of different lengths describe two different blocks.
        let mut output = AudioStorage::<f32>::new(1, 4);
        let error = host.process(gain, &input, &mut output).unwrap_err();
        assert_eq!(error.kind(), HostErrorKind::BadBlock);
        assert!(
            error.message().contains("one block is one length"),
            "{error}"
        );
    }

    /// An instrument gets no input bus at all, and must still see a block of the right
    /// length rather than whatever the host's output buffer contained.
    #[test]
    fn a_plug_in_with_no_input_is_given_a_block_and_not_a_bus() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");

        let mut output = AudioStorage::<f32>::new(2, 16);
        output.fill(7.0);
        let silent_input = AudioStorage::<f32>::new(0, 0);
        host.process(gain, &silent_input, &mut output)
            .expect("a block with no input");
        assert!(
            output.as_slice().iter().all(|sample| *sample == 0.0),
            "an instrument writes its own output, it does not inherit the host's"
        );
    }

    #[test]
    fn state_round_trips_and_a_bad_blob_is_refused_without_changing_anything() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");

        host.set_param(gain, GAIN_ID.0, 3.5);
        host.set_param(gain, BYPASS_ID.0, 1.0);
        let saved = host.save_state(gain).expect("saves");
        assert!(!saved.is_empty());

        host.set_param(gain, GAIN_ID.0, 0.25);
        host.set_param(gain, BYPASS_ID.0, 0.0);
        host.load_state(gain, &saved).expect("loads");
        assert_eq!(host.param_value(gain, GAIN_ID.0).expect("known"), 3.5);
        assert_eq!(host.param_value(gain, BYPASS_ID.0).expect("known"), 1.0);

        // `abi-v1` §12: a blob the plug-in cannot read must fail with no side effects.
        let error = host
            .load_state(gain, b"not a state blob at all")
            .expect_err("refused");
        assert_eq!(error.kind(), HostErrorKind::Plugin);
        assert_eq!(
            host.param_value(gain, GAIN_ID.0).expect("known"),
            3.5,
            "a refused load must not have moved anything"
        );

        // A truncated blob is the other half of the same case.
        let truncated = &saved[..saved.len() / 2];
        assert!(host.load_state(gain, truncated).is_err());
        assert_eq!(host.param_value(gain, GAIN_ID.0).expect("known"), 3.5);
    }

    /// The whole point of a state blob: a preset saved by one instance loads into another.
    #[test]
    fn state_moves_between_two_instances() {
        let mut host = host();
        let first = host.install::<GainPlugin>().expect("installs");
        let second = host.install::<GainPlugin>().expect("installs");

        host.set_param(first, GAIN_ID.0, 2.75);
        let preset = host.save_state(first).expect("saves");
        host.load_state(second, &preset).expect("loads");

        assert_eq!(host.param_value(second, GAIN_ID.0).expect("known"), 2.75);
        assert_eq!(
            host.param_value(first, GAIN_ID.0).expect("known"),
            2.75,
            "two instances must not share parameter storage"
        );

        host.set_param(second, GAIN_ID.0, 1.0);
        assert_eq!(
            host.param_value(first, GAIN_ID.0).expect("known"),
            2.75,
            "changing one instance must not change the other"
        );
    }

    /// `set_host` is where a plug-in reports latency and asks for what it needs, so the
    /// harness has to have handed over real services by then.
    #[test]
    fn the_plug_in_is_given_services_that_answer() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");

        assert_eq!(
            host.host().latency(),
            64,
            "the plug-in reported its latency through `daux.host.latency/1`"
        );
        assert_eq!(host.host().latency_reports(), 1);
        assert_eq!(host.host().tail_changes(), 1);
        assert_eq!(host.latency(gain).expect("known"), Latency::Samples(64));
        assert_eq!(host.tail(gain).expect("known"), Tail::Samples(128));
    }

    #[test]
    fn worker_tasks_are_queued_by_the_host_and_run_on_demand() {
        let (mut host, gain, probe) = host_with_probe();

        // The plug-in would do this from `process`; doing it directly keeps the test about
        // the harness rather than about the fixture.
        assert!(HostWorker::schedule(host.host().as_ref(), TaskId(7)));
        assert!(HostWorker::schedule(host.host().as_ref(), TaskId(8)));
        assert_eq!(Probe::count(&probe.main_thread_calls), 0);

        let ran = host.run_callbacks(gain).expect("known");
        assert_eq!(ran, [TaskId(7), TaskId(8)]);
        assert_eq!(
            probe.tasks(),
            [TaskId(7), TaskId(8)],
            "the tasks must reach the plug-in, in the order it queued them"
        );
        assert_eq!(Probe::count(&probe.main_thread_calls), 1);

        assert!(
            host.run_callbacks(gain).expect("known").is_empty(),
            "a task runs once"
        );
        assert_eq!(probe.tasks().len(), 2);
    }

    #[test]
    fn a_plug_in_that_refuses_to_be_prepared_is_reported_rather_than_installed() {
        let mut host = host();
        let error = host.install::<RefusingPlugin>().expect_err("refused");
        assert_eq!(error.kind(), HostErrorKind::Unsupported);
        assert_eq!(
            host.instance_count(),
            0,
            "an instance that could not be prepared must not be left half-installed"
        );
    }

    /// A stale handle must never reach someone else's plug-in.
    #[test]
    fn an_unloaded_instance_is_gone_and_its_id_is_never_reused() {
        let mut host = host();
        let first = host.install::<GainPlugin>().expect("installs");
        let second = host.install::<GainPlugin>().expect("installs");
        assert_ne!(first, second);

        host.unload(first).expect("unloads");
        assert_eq!(host.instance_count(), 1);
        assert_eq!(
            host.unload(first).unwrap_err().kind(),
            HostErrorKind::NoSuchInstance,
            "unloading twice is not allowed to succeed quietly"
        );

        let input = ramp(1, 4);
        let mut output = AudioStorage::<f32>::new(1, 4);
        assert_eq!(
            host.process(first, &input, &mut output).unwrap_err().kind(),
            HostErrorKind::NoSuchInstance
        );
        assert!(host.param_value(first, GAIN_ID.0).is_err());
        assert!(host.save_state(first).is_err());
        assert!(host.descriptor(first).is_err());

        // The surviving instance is untouched, and the next id is a new one.
        let third = host.install::<GainPlugin>().expect("installs");
        assert_ne!(third, first);
        host.process(second, &input, &mut output)
            .expect("still fine");
    }

    /// The steady-time counter is what tells a plug-in that two blocks are two blocks.
    #[test]
    fn the_steady_time_counter_advances_by_the_block_length() {
        let mut host = host();
        let gain = host.install::<GainPlugin>().expect("installs");
        assert_eq!(host.steady_time(), 0);

        let input = ramp(1, 32);
        let mut output = AudioStorage::<f32>::new(1, 32);
        host.process(gain, &input, &mut output).expect("a block");
        assert_eq!(host.steady_time(), 32);
        host.process(gain, &input, &mut output).expect("a block");
        assert_eq!(host.steady_time(), 64);
    }

    /// `abi-v1` §7's lifecycle, driven by the harness and observed from inside the plug-in:
    /// prepared once, activated once, and deactivated exactly once when it goes away. A
    /// harness that forgot the last one would leave a plug-in holding its DSP resources
    /// after the host thought it was gone.
    #[test]
    fn the_lifecycle_is_driven_in_the_order_the_abi_requires() {
        let (mut host, gain, probe) = host_with_probe();
        assert_eq!(Probe::count(&probe.prepares), 1);
        assert_eq!(Probe::count(&probe.activations), 1);
        assert_eq!(Probe::count(&probe.deactivations), 0);
        assert_eq!(
            Probe::count(&probe.max_block_size),
            512,
            "the plug-in is told how big a block to expect, once, before it runs"
        );
        assert!(probe.has_host.load(std::sync::atomic::Ordering::Relaxed));

        let input = ramp(1, 4);
        let mut output = AudioStorage::<f32>::new(1, 4);
        host.process(gain, &input, &mut output).expect("a block");
        host.process(gain, &input, &mut output).expect("a block");
        assert_eq!(Probe::count(&probe.blocks), 2);

        host.reset(gain).expect("resets");
        assert_eq!(Probe::count(&probe.resets), 1);
        assert_eq!(
            Probe::count(&probe.prepares),
            1,
            "a reset is not a re-prepare"
        );

        host.unload(gain).expect("unloads");
        assert_eq!(
            Probe::count(&probe.deactivations),
            1,
            "an unloaded plug-in must be deactivated, not merely dropped"
        );
    }

    /// The same obligation on the other exit: dropping the whole host must not leak an
    /// active plug-in.
    #[test]
    fn dropping_the_host_deactivates_what_it_was_driving() {
        let (host, _gain, probe) = host_with_probe();
        assert_eq!(Probe::count(&probe.deactivations), 0);
        drop(host);
        assert_eq!(Probe::count(&probe.deactivations), 1);
    }

    #[test]
    fn loading_something_that_is_not_a_bundle_fails_without_panicking() {
        let mut host = host();
        let temp = std::env::temp_dir().join("daux-host-not-a-bundle");
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("a temp directory");

        for candidate in [
            temp.join("Nothing.axt"),
            temp.clone(),
            temp.join("file.axt"),
        ] {
            if candidate.extension().is_some() && candidate.to_string_lossy().ends_with("file.axt")
            {
                std::fs::write(&candidate, b"not a directory").expect("write");
            }
            let error = host.load(&candidate).expect_err("not a bundle");
            assert_eq!(error.kind(), HostErrorKind::Load, "{error}");
            assert!(error.path().is_some(), "a load failure must name the path");
        }
        assert_eq!(host.instance_count(), 0);

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// A bundle that is well formed but ships nothing this machine can run is a normal
    /// thing to own; the harness must say so rather than report corruption.
    #[test]
    fn a_bundle_with_no_binary_for_this_machine_is_reported_as_a_load_failure() {
        use daux_runtime::daux_bundle::{BundleBuilder, TargetId};

        let temp = std::env::temp_dir().join("daux-host-foreign-bundle");
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("a temp directory");
        let stub = temp.join("libgain.so");
        std::fs::write(&stub, b"for another machine").expect("write");

        let root = BundleBuilder::new("com.example.gain", "Gain", "Example", "1.0.0")
            .expect("a valid identity")
            .binary(
                TargetId::parse("aix-power64").expect("valid syntax, never the host"),
                &stub,
            )
            .write(&temp)
            .expect("the bundle writes");

        let mut host = host();
        let error = host.load(&root).expect_err("nothing to load here");
        assert_eq!(error.kind(), HostErrorKind::Load);
        assert_eq!(host.instance_count(), 0);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn the_harness_records_what_the_plug_in_asked_the_host_for() {
        let host = host();
        let recorder = host.host();
        HostLog::log(
            recorder.as_ref(),
            daux_runtime::daux_core::daux_rt::LogLevel::Warn,
            "voice steal",
        );
        let log = recorder.drain_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].message, "voice steal");
    }

    #[test]
    fn the_harness_can_be_moved_to_another_thread() {
        const fn assert_send<T: Send>() {}
        assert_send::<TestHost>();
        assert_send::<InstanceId>();

        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HarnessHost>();
        assert_send_sync::<HostError>();
    }
}
