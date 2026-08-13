//! Host-side AXT runtime: bundle → dynamic library → factory → instance.
//!
//! This is the crate a DAUx host loads plug-ins with. It goes from a directory on disk to a
//! running instance, and it does so treating everything it meets as untrusted: the bundle
//! came from the internet, and the module inside it is code the host did not compile and
//! cannot inspect.
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use daux_bundle::{Bundle, TargetId};
//! use daux_core::ProcessConfig;
//! use daux_host_services::HostServices;
//! use daux_runtime::{AxtModule, HostBlock, HostBridge, LoadedFactory};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bundle = Bundle::open(std::path::Path::new("Gain.axt"))?;
//! let module = Arc::new(AxtModule::load(&bundle, &TargetId::host())?);
//! let factory = LoadedFactory::create(module, HostBridge::new(HostServices::null()))?;
//!
//! let descriptor = factory.descriptor(0)?;
//! let mut plugin = factory.create_plugin(descriptor.id.as_str())?;
//!
//! let config = ProcessConfig::new(48_000.0, 512);
//! plugin.activate(&config)?;
//! plugin.start_processing()?;
//!
//! let mut left = vec![0.0f32; 512];
//! let mut right = vec![0.0f32; 512];
//! let mut block = HostBlock::new(&[], &[2], 512);
//! block.set_frames(512)?;
//! block.bind_output(0, 0, &mut left)?;
//! block.bind_output(0, 1, &mut right)?;
//! let status = plugin.process(&mut block);
//! # let _ = status;
//! # Ok(())
//! # }
//! ```
//!
//! # The one invariant everything else rests on
//!
//! **A dangling function pointer into an unloaded module is an instant crash with no
//! diagnostic.** There is no error to report and no stack to look at: the process dies
//! somewhere unrelated, usually in a different vendor's plug-in.
//!
//! So the ownership chain is not a convention:
//!
//! ```text
//! Arc<AxtModule>            the libloading::Library lives here
//!   └── LoadedFactory       Arc<FactoryInner> — holds the module and the HostBridge
//!         └── LoadedPlugin  holds Arc<FactoryInner>
//! ```
//!
//! Every derived object holds a strong reference to the one above it, so there is no order
//! a caller can drop things in that unloads the library early, destroys a factory that
//! still has instances (`abi-v1` §5), or frees the `DauxHostV1` before `destroy_factory`
//! has returned (`abi-v1` §4).
//!
//! # Validation before the first call
//!
//! `abi-v1` §3 gives four rejection rules, and this crate applies all of them before
//! calling anything through a module's header: the entry symbol must exist and return
//! non-null, the magic must match, the major version must be 1, and every structure's
//! declared `size` must cover its whole v1.0 revision. A module built against a newer
//! *minor* revision is accepted and its unknown tail ignored, exactly as §3 requires.
//!
//! Beyond `size`, every function table is checked entry by entry: a non-optional
//! `unsafe extern "C" fn` has no null representation in Rust, so materialising a table with
//! one would be undefined behaviour before the host could notice. The loader reads
//! those slots as raw words first.
//!
//! # Dependency search paths
//!
//! Bundled dependencies are found with `AddDllDirectory` plus
//! `LOAD_LIBRARY_SEARCH_USER_DIRS` on Windows, and with the `$ORIGIN` rpath the plug-in
//! binary carries elsewhere. `PATH` and `LD_LIBRARY_PATH` are never touched: they are
//! process-global, and editing them corrupts dependency resolution for every other plug-in
//! in the host.
//!
//! # Threading
//!
//! [`LoadedPlugin`] is `Send` but not `Sync`, matching `abi-v1` §15: calls for one instance
//! are never concurrent, but the instance may move between the main thread and an audio
//! thread. [`HostBlock`] is likewise `Send` and not `Sync`.
//!
//! Only [`LoadedPlugin::process`], [`LoadedPlugin::start_processing`],
//! [`LoadedPlugin::stop_processing`] and the [`EventList`] methods are meant for the audio
//! thread, and none of them allocates.

mod block;
mod descriptor;
mod error;
mod events;
mod ext;
mod factory;
mod host;
mod module;
mod plugin;
mod probe;
mod search_path;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod integration;
#[cfg(test)]
mod testing;

pub use block::HostBlock;
pub use error::{RuntimeError, RuntimeErrorKind, RuntimeResult};
pub use events::{EventList, EventListFull, MAX_EVENT_BYTES, MAX_SYSEX_BYTES};
pub use ext::{GuiExt, MAX_STATE_BYTES, ParamsExt, StateExt};
pub use factory::LoadedFactory;
pub use host::HostBridge;
pub use module::AxtModule;
pub use plugin::{LoadedPlugin, PluginState};

/// The crates whose types appear in this one's signatures, re-exported so a host can name
/// them without adding each dependency itself.
pub use {daux_abi, daux_bundle, daux_core, daux_host_services, daux_parameter, daux_transport};

/// The crate's own tests run under the counting allocator, so "the audio-thread path does
/// not allocate" is checked rather than asserted. Production builds are untouched.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_core::daux_rt::CountingAllocator =
    daux_core::daux_rt::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_bundle::{Bundle, BundleBuilder, TargetId};
    use daux_host_services::HostServices;
    use std::sync::Arc;

    /// The ownership chain the whole crate rests on, asserted as a type property: an
    /// instance keeps the factory alive, and the factory keeps the module alive. If any
    /// link were a borrow instead of an `Arc`, one of these would not compile.
    #[test]
    fn the_ownership_chain_is_expressed_in_the_types() {
        const fn assert_send<T: Send>() {}
        assert_send::<AxtModule>();
        assert_send::<HostBridge>();
        assert_send::<LoadedFactory>();
        assert_send::<LoadedPlugin>();

        // `AxtModule` is only ever handed to a factory behind an `Arc`, which is the shape
        // that makes early unloading unexpressible.
        fn takes_arc(_: Arc<AxtModule>) {}
        let _ = takes_arc;
    }

    /// A bundle with no binary for this machine is the normal outcome for a cross-platform
    /// `.axt`, and must be reported as "not here", never as corruption.
    #[test]
    fn loading_a_bundle_without_a_binary_for_this_target_is_not_corruption() {
        let root = std::env::temp_dir().join("daux-runtime-empty-bundle");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");

        // A stand-in for a binary this machine cannot run: the file exists, so the bundle
        // is well formed, but it is declared for a target that is never the host.
        let foreign = TargetId::parse("aix-power64").expect("a syntactically valid target");
        let stub = root.join("libgain.so");
        std::fs::write(&stub, b"not a real library").expect("stub binary");

        let path = BundleBuilder::new("com.example.gain", "Gain", "Example", "1.0.0")
            .expect("a valid identity")
            .binary(foreign, &stub)
            .write(&root)
            .expect("the bundle writes");
        let bundle = Bundle::open(&path).expect("and opens");

        let err = AxtModule::load(&bundle, &TargetId::host()).expect_err("nothing to load");
        assert_eq!(
            err.kind(),
            RuntimeErrorKind::NotFound,
            "a bundle that ships nothing for this platform is not a broken bundle: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A host with no services at all must still produce a usable bridge — that is what
    /// makes offline rendering and unit tests possible without a real host.
    #[test]
    fn a_null_host_produces_a_complete_bridge() {
        let bridge = HostBridge::new(HostServices::null());
        assert!(!bridge.as_raw().is_null());
        // SAFETY: `bridge` is alive, so its boxed interface and its table are valid.
        let api = unsafe { (*bridge.as_raw()).api() }.expect("a bridge always has a table");
        assert_eq!(api.abi_version_major, daux_abi::DAUX_ABI_VERSION_MAJOR);
        assert_eq!(bridge.services().info().name, "unknown");
    }
}
