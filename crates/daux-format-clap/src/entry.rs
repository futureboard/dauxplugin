//! `clap_entry`, the one symbol a CLAP binary exports, and the factory behind it.
//!
//! # Why a registry
//!
//! `clap_plugin_factory` has no user-data field: its three functions get only a pointer to
//! the table itself. The plug-in set therefore has to come from somewhere the functions can
//! reach without an argument, and the only such thing is the type parameter — which is why
//! every function here is generic over the [`DauxFactory`] and why the tables are built per
//! monomorphisation.
//!
//! CLAP also requires the descriptor pointers a factory hands out to stay valid until
//! `clap_plugin_entry::deinit`, so the descriptors are built once and kept for the life of
//! the process. That is module-level, write-once, immortal state, which is exactly what
//! abi-v1 §16.1 describes for an entry — it is not per-instance state, and hundreds of
//! instances of the same plug-in still share nothing mutable.
//!
//! # Panics
//!
//! Every exported function wraps its body in `catch_unwind` and converts a panic into the
//! failure value CLAP expects: `false`, a null pointer, or zero (abi-v1 §17).

use core::any::TypeId;
use core::ffi::{CStr, c_char, c_void};
use core::marker::PhantomData;
use core::panic::AssertUnwindSafe;
use core::ptr;
use std::collections::HashMap;
use std::panic::catch_unwind;
use std::sync::{Mutex, OnceLock};

use daux_plugin_api::DauxFactory;

use crate::abi::{
    CLAP_PLUGIN_FACTORY_ID, ClapHost, ClapPlugin, ClapPluginDescriptor, ClapPluginEntry,
    ClapPluginFactory, ClapVersion,
};
use crate::descriptor::OwnedDescriptor;
use crate::plugin::ClapInstance;
use crate::text::borrow_str;

/// Runs `f`, converting a panic into `fallback` rather than letting it cross the boundary.
///
/// `AssertUnwindSafe` is the honest claim: a panic escaping a factory function leaves
/// nothing observable behind, because the only state it could have touched is the
/// write-once registry, which is rebuilt from scratch on the next call if it was never
/// installed.
fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

/// Everything one exported factory needs, built once and never freed.
struct Registry {
    /// The table `get_factory` hands to the host.
    factory: &'static ClapPluginFactory,
    /// One owned descriptor per plug-in, in factory index order.
    descriptors: &'static [&'static OwnedDescriptor],
}

/// The registry for `F`, building it on first use.
///
/// Keyed by [`TypeId`] rather than kept in one slot, so a binary that exports one factory
/// and a test process that exercises several behave the same way.
///
/// `[main-thread]` — CLAP marks `get_factory` and every factory method main-thread, and the
/// lock is only ever taken from them.
fn registry<F: DauxFactory + Default>() -> &'static Registry {
    static CACHE: OnceLock<Mutex<HashMap<TypeId, &'static Registry>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = TypeId::of::<F>();

    // A poisoned mutex here would mean a previous caller panicked while holding it. The map
    // is only ever inserted into, so its contents are still consistent and recovering is
    // strictly better than turning one panic into a permanently unloadable plug-in.
    if let Some(found) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return found;
    }

    let descriptors: Vec<&'static OwnedDescriptor> = F::default()
        .descriptors()
        .into_iter()
        .map(|d| &*Box::leak(Box::new(OwnedDescriptor::new(d))))
        .collect();
    let registry: &'static Registry = Box::leak(Box::new(Registry {
        factory: Box::leak(Box::new(ClapPluginFactory {
            get_plugin_count: factory_count::<F>,
            get_plugin_descriptor: factory_descriptor::<F>,
            create_plugin: factory_create::<F>,
        })),
        descriptors: Box::leak(descriptors.into_boxed_slice()),
    }));

    // Two threads racing to build the same registry is legal but wasteful; the loser's
    // allocation is simply never used, and both callers see the same table afterwards.
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key)
        .or_insert(registry)
}

unsafe extern "C" fn factory_count<F: DauxFactory + Default>(
    _factory: *const ClapPluginFactory,
) -> u32 {
    guard(0, || {
        u32::try_from(registry::<F>().descriptors.len()).unwrap_or(u32::MAX)
    })
}

unsafe extern "C" fn factory_descriptor<F: DauxFactory + Default>(
    _factory: *const ClapPluginFactory,
    index: u32,
) -> *const ClapPluginDescriptor {
    guard(ptr::null(), || {
        registry::<F>()
            .descriptors
            .get(index as usize)
            .map_or(ptr::null(), |d| d.view())
    })
}

unsafe extern "C" fn factory_create<F: DauxFactory + Default>(
    _factory: *const ClapPluginFactory,
    host: *const ClapHost,
    plugin_id: *const c_char,
) -> *const ClapPlugin {
    guard(ptr::null(), || {
        // SAFETY: CLAP passes a NUL-terminated plug-in id valid for the call, or null.
        let Some(id) = (unsafe { borrow_str(plugin_id) }) else {
            return ptr::null();
        };
        let registry = registry::<F>();
        // The descriptor the instance will publish must be *this* factory's, not a fresh
        // one: the host is already holding the pointer it got from `get_plugin_descriptor`
        // and compares them.
        let Some(descriptor) = registry.descriptors.iter().copied().find(|d| d.id() == id) else {
            return ptr::null();
        };
        let Ok(plugin) = F::default().create(id) else {
            return ptr::null();
        };
        // SAFETY: `host` is whatever CLAP passed — null or a `clap_host` the host keeps
        // alive until the instance is destroyed, which is `create`'s requirement. The
        // returned pointer is handed straight to the host, which releases it through
        // `clap_plugin::destroy`.
        unsafe { ClapInstance::create(plugin, descriptor, host) }
    })
}

unsafe extern "C" fn entry_init(_plugin_path: *const c_char) -> bool {
    // Deliberately does no work, and deliberately is not generic: a host may call
    // `init`/`deinit` several times in pairs, and anything built here would need a reference
    // count to survive that. The registry is built lazily on first use instead, which needs
    // no counting and no per-factory initialisation at all.
    true
}

unsafe extern "C" fn entry_deinit() {
    // Nothing to undo: see `entry_init`. The descriptors deliberately outlive `deinit`,
    // because a host is allowed to keep reading a descriptor pointer up to that point, and
    // getting the ordering wrong is a use-after-free inside the host's scanner.
}

unsafe extern "C" fn entry_get_factory<F: DauxFactory + Default>(
    factory_id: *const c_char,
) -> *const c_void {
    guard(ptr::null(), || {
        if factory_id.is_null() {
            return ptr::null();
        }
        // SAFETY: CLAP passes a NUL-terminated factory id valid for the call.
        let id = unsafe { CStr::from_ptr(factory_id) };
        if id != CLAP_PLUGIN_FACTORY_ID {
            // An unknown factory id — a preset discovery factory, say — must produce null
            // rather than a table of the wrong shape.
            return ptr::null();
        }
        ptr::from_ref(registry::<F>().factory).cast()
    })
}

/// The `clap_entry` value for one [`DauxFactory`].
///
/// Not used directly: [`export_entry!`](crate::export_entry) writes the exported symbol.
/// It is public so the macro can name it, and so a test can drive the entry points without
/// defining a `#[unsafe(no_mangle)]` symbol.
///
/// `[main-thread]`
pub struct ClapEntry<F>(PhantomData<F>);

impl<F: DauxFactory + Default> ClapEntry<F> {
    /// The table a CLAP host looks for under the symbol `clap_entry`.
    pub const ENTRY: ClapPluginEntry = ClapPluginEntry {
        clap_version: ClapVersion::CURRENT,
        init: entry_init,
        deinit: entry_deinit,
        get_factory: entry_get_factory::<F>,
    };
}

/// Exports `$factory` as this binary's `clap_entry`.
///
/// The factory type must implement [`DauxFactory`] and [`Default`]; a CLAP factory table
/// carries no state of its own, so the type is the only thing the entry points have to work
/// from.
///
/// ```ignore
/// daux_format_clap::export_entry!(MyFactory);
/// ```
///
/// A binary may contain exactly one of these, because a shared library may contain exactly
/// one `clap_entry` symbol.
#[macro_export]
macro_rules! export_entry {
    ($factory:ty) => {
        /// The CLAP entry point of this binary.
        #[allow(non_upper_case_globals)]
        #[unsafe(no_mangle)]
        pub static clap_entry: $crate::abi::ClapPluginEntry = <$crate::ClapEntry<$factory>>::ENTRY;
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{
        CLAP_EXT_AUDIO_PORTS, CLAP_EXT_GUI, CLAP_EXT_NOTE_PORTS, CLAP_EXT_PARAMS, CLAP_EXT_STATE,
        CLAP_PROCESS_CONTINUE, CLAP_PROCESS_ERROR,
    };
    use crate::testkit::{
        EventList, FailingFactory, PanicPoint, PanickingFactory, TestFactory, TestHost, TestStream,
        read_c,
    };

    /// Drives an entry table the way a host does: version check, `init`, `get_factory`.
    fn open<F: DauxFactory + Default>() -> &'static ClapPluginFactory {
        let entry = ClapEntry::<F>::ENTRY;
        assert!(entry.clap_version.is_compatible());
        // SAFETY: `init` and `get_factory` are this crate's own functions and tolerate a
        // null path; the returned pointer is a `'static` table.
        unsafe {
            assert!((entry.init)(c"/tmp/Test.clap".as_ptr()));
            let raw = (entry.get_factory)(CLAP_PLUGIN_FACTORY_ID.as_ptr());
            assert!(!raw.is_null());
            &*raw.cast::<ClapPluginFactory>()
        }
    }

    #[test]
    fn the_entry_publishes_only_the_plugin_factory() {
        let entry = ClapEntry::<TestFactory>::ENTRY;
        // SAFETY: the entry's functions are this crate's own and take only C strings.
        unsafe {
            assert!((entry.init)(ptr::null()));
            assert!(!(entry.get_factory)(CLAP_PLUGIN_FACTORY_ID.as_ptr()).is_null());
            assert!((entry.get_factory)(c"clap.preset-discovery-factory".as_ptr()).is_null());
            assert!((entry.get_factory)(ptr::null()).is_null());
            (entry.deinit)();
        }
    }

    #[test]
    fn the_factory_enumerates_descriptors_and_refuses_out_of_range_indices() {
        let factory = open::<TestFactory>();
        // SAFETY: `factory` is a live table; every call below only reads it.
        unsafe {
            assert_eq!((factory.get_plugin_count)(factory), 1);
            let d = (factory.get_plugin_descriptor)(factory, 0);
            assert!(!d.is_null());
            assert_eq!(read_c((*d).id), "com.example.clap-test");
            assert_eq!(read_c((*d).name), "CLAP Test");
            assert_eq!(read_c((*d).vendor), "Example Audio");
            assert!((factory.get_plugin_descriptor)(factory, 1).is_null());
            assert!((factory.get_plugin_descriptor)(factory, u32::MAX).is_null());
        }
    }

    #[test]
    fn the_descriptor_pointer_is_stable_across_calls() {
        let factory = open::<TestFactory>();
        // SAFETY: `factory` is a live table.
        let (a, b) = unsafe {
            (
                (factory.get_plugin_descriptor)(factory, 0),
                (factory.get_plugin_descriptor)(factory, 0),
            )
        };
        assert_eq!(
            a, b,
            "a host caches the pointer and compares it by identity"
        );
    }

    #[test]
    fn an_unknown_plugin_id_and_a_null_one_produce_no_instance() {
        let factory = open::<TestFactory>();
        let host = TestHost::new();
        // SAFETY: `factory` and the fake host outlive the calls.
        unsafe {
            assert!(
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.no".as_ptr())
                    .is_null()
            );
            assert!((factory.create_plugin)(factory, host.as_ptr(), ptr::null()).is_null());
        }
    }

    #[test]
    fn a_factory_that_refuses_to_build_produces_null_rather_than_a_broken_instance() {
        let factory = open::<FailingFactory>();
        let host = TestHost::new();
        // SAFETY: as above.
        unsafe {
            assert_eq!((factory.get_plugin_count)(factory), 1);
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-fail".as_ptr());
            assert!(plugin.is_null());
        }
    }

    #[test]
    fn a_full_lifecycle_runs_through_the_c_entry_points() {
        let factory = open::<TestFactory>();
        let host = TestHost::new();
        // SAFETY: every pointer below came from this adapter or from the fake host, and
        // each is used only while it is alive. The sequence is the one CLAP prescribes.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            assert!(!plugin.is_null());
            let p = &*plugin;

            assert!((p.init)(plugin));
            assert!(!(p.get_extension)(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr()).is_null());
            assert!(!(p.get_extension)(plugin, CLAP_EXT_PARAMS.as_ptr()).is_null());
            assert!(!(p.get_extension)(plugin, CLAP_EXT_STATE.as_ptr()).is_null());
            assert!(!(p.get_extension)(plugin, CLAP_EXT_NOTE_PORTS.as_ptr()).is_null());
            assert!(
                (p.get_extension)(plugin, c"clap.audio-ports-config".as_ptr()).is_null(),
                "an extension this adapter does not implement must answer null"
            );
            assert!((p.get_extension)(plugin, ptr::null()).is_null());

            assert!((p.activate)(plugin, 48_000.0, 0, 128));
            assert!((p.start_processing)(plugin));

            let mut block = host.block(2, 2, 64);
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_CONTINUE);
            assert_eq!(
                block.output(0, 0),
                &[1.0f32; 64][..],
                "the test plug-in copies its input through"
            );

            (p.reset)(plugin);
            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            (p.on_main_thread)(plugin);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn calls_made_out_of_order_are_refused_rather_than_obeyed() {
        let factory = open::<TestFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            let p = &*plugin;
            assert!((p.init)(plugin));

            // Processing before activation must not reach the plug-in.
            let mut block = host.block(2, 2, 32);
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_ERROR);
            assert_eq!(
                block.output(0, 0),
                &[0.0f32; 32][..],
                "a refusal must silence"
            );

            // Zero maximum block size leaves every buffer unsized, so it is refused.
            assert!(!(p.activate)(plugin, 48_000.0, 0, 0));
            // A second `init` is refused, and the instance stays usable.
            assert!(!(p.init)(plugin));

            assert!((p.activate)(plugin, 48_000.0, 0, 64));
            assert!(!(p.activate)(plugin, 48_000.0, 0, 64), "already active");
            assert!((p.start_processing)(plugin));

            // A block longer than the activation promised is refused and silenced, because
            // the plug-in sized its buffers from that promise.
            let mut long = host.block(2, 2, 65);
            assert_eq!((p.process)(plugin, long.as_ptr()), CLAP_PROCESS_ERROR);
            assert_eq!(long.output(0, 0), &[0.0f32; 65][..]);

            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_null_process_pointer_is_an_error_and_not_a_crash() {
        let factory = open::<TestFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test; the null is the input under test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            let p = &*plugin;
            assert!((p.init)(plugin));
            assert_eq!((p.process)(plugin, ptr::null()), CLAP_PROCESS_ERROR);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_headless_plugin_does_not_publish_the_gui_extension() {
        let factory = open::<TestFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            let p = &*plugin;
            assert!((p.init)(plugin));
            assert!(
                (p.get_extension)(plugin, CLAP_EXT_GUI.as_ptr()).is_null(),
                "publishing clap.gui without HAS_GUI gives the user an empty window"
            );
            (p.destroy)(plugin);
        }
    }

    /// The test the whole crate exists to pass: a plug-in that panics must not unwind into
    /// the host, must report the failure, and must refuse every later call.
    #[test]
    fn a_panicking_process_is_caught_and_poisons_the_instance() {
        let factory = open::<PanickingFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-panic".as_ptr());
            assert!(!plugin.is_null());
            let p = &*plugin;
            assert!((p.init)(plugin));
            assert!((p.activate)(plugin, 48_000.0, 0, 64));
            assert!((p.start_processing)(plugin));

            PanicPoint::arm_process();
            let mut block = host.block(2, 2, 32);
            // The panic is caught at the boundary and reported as a process error.
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_ERROR);

            // Everything afterwards is refused, whether or not the plug-in would panic
            // again — the instance is poisoned, not merely unlucky.
            PanicPoint::disarm();
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_ERROR);
            assert!(!(p.start_processing)(plugin));
            assert!(!(p.activate)(plugin, 48_000.0, 0, 64));

            let params = (p.get_extension)(plugin, CLAP_EXT_PARAMS.as_ptr())
                .cast::<crate::abi::ClapPluginParams>();
            assert!(!params.is_null());
            assert_eq!(
                ((*params).count)(plugin),
                0,
                "a poisoned instance must not report parameters it can no longer serve"
            );

            // …and it can still be destroyed, which is the one thing a host must be able to
            // do with it (abi-v1 §17).
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_panic_in_a_main_thread_call_is_caught_and_poisons_too() {
        let factory = open::<PanickingFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-panic".as_ptr());
            let p = &*plugin;
            assert!((p.init)(plugin));

            let params = &*(p.get_extension)(plugin, CLAP_EXT_PARAMS.as_ptr())
                .cast::<crate::abi::ClapPluginParams>();
            assert_eq!((params.count)(plugin), 1);

            PanicPoint::arm_params();
            let mut value = 0.0f64;
            assert!(!(params.get_value)(plugin, 1, &raw mut value));

            PanicPoint::disarm();
            assert_eq!(
                (params.count)(plugin),
                0,
                "the panic must have poisoned the instance, not just failed one call"
            );
            assert!(!(p.activate)(plugin, 48_000.0, 0, 64));
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_panic_while_the_lock_is_held_still_releases_it() {
        let factory = open::<PanickingFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-panic".as_ptr());
            let p = &*plugin;
            assert!((p.init)(plugin));
            PanicPoint::arm_params();
            let params = &*(p.get_extension)(plugin, CLAP_EXT_PARAMS.as_ptr())
                .cast::<crate::abi::ClapPluginParams>();
            let mut value = 0.0f64;
            assert!(!(params.get_value)(plugin, 1, &raw mut value));
            PanicPoint::disarm();
            // If the lock had leaked, this call would spin for ten thousand yields and then
            // refuse; it returns promptly because the guard released while unwinding.
            assert_eq!((params.count)(plugin), 0);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn two_instances_of_one_plugin_are_independent() {
        let factory = open::<TestFactory>();
        let host = TestHost::new();
        // SAFETY: as in the lifecycle test.
        unsafe {
            let a =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            let b =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            assert!(!a.is_null() && !b.is_null());
            assert_ne!(a, b);
            assert_eq!((*a).desc, (*b).desc, "one descriptor, many instances");

            assert!(((*a).init)(a));
            assert!(((*b).init)(b));

            let params_a = &*((*a).get_extension)(a, CLAP_EXT_PARAMS.as_ptr())
                .cast::<crate::abi::ClapPluginParams>();
            let mut parsed = 0.0f64;
            assert!((params_a.text_to_value)(
                a,
                1,
                c"0.25".as_ptr(),
                &raw mut parsed
            ));
            assert_eq!(parsed, 0.25);

            // Only `a` is activated; `b` must stay inactive and refuse to process.
            assert!(((*a).activate)(a, 48_000.0, 0, 64));
            assert!(((*a).start_processing)(a));
            let mut block = host.block(2, 2, 16);
            assert_eq!(((*a).process)(a, block.as_ptr()), CLAP_PROCESS_CONTINUE);
            assert_eq!(((*b).process)(b, block.as_ptr()), CLAP_PROCESS_ERROR);

            ((*a).stop_processing)(a);
            ((*a).deactivate)(a);
            ((*a).destroy)(a);
            ((*b).destroy)(b);
        }
    }

    // ---- extensions -------------------------------------------------------------------

    /// An initialised instance of [`TestFactory`]'s plug-in, ready for extension calls.
    ///
    /// # Safety
    ///
    /// The returned pointer must be released with `plugin->destroy` before the test ends.
    unsafe fn instance(host: &TestHost) -> *const ClapPlugin {
        let factory = open::<TestFactory>();
        // SAFETY: `factory` and `host` are both live, and the id is one the factory knows.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, host.as_ptr(), c"com.example.clap-test".as_ptr());
            assert!(!plugin.is_null());
            assert!(((*plugin).init)(plugin));
            plugin
        }
    }

    /// Fetches an extension table, asserting it is published.
    ///
    /// # Safety
    ///
    /// `plugin` must be a live instance and `T` the struct the id names.
    unsafe fn ext<T>(plugin: *const ClapPlugin, id: &CStr) -> &'static T {
        // SAFETY: the caller pairs the id with the right struct, and every table this
        // adapter publishes is a `'static` item.
        unsafe {
            let raw = ((*plugin).get_extension)(plugin, id.as_ptr());
            assert!(!raw.is_null(), "extension not published");
            &*raw.cast::<T>()
        }
    }

    #[test]
    fn audio_ports_describe_the_plugins_bus_layout() {
        use crate::abi::{
            CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, CLAP_INVALID_ID, ClapAudioPortInfo,
            ClapPluginAudioPorts,
        };
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let ports: &ClapPluginAudioPorts = ext(plugin, CLAP_EXT_AUDIO_PORTS);
            assert_eq!((ports.count)(plugin, true), 1);
            assert_eq!((ports.count)(plugin, false), 1);

            let mut info: ClapAudioPortInfo = core::mem::zeroed();
            assert!((ports.get)(plugin, 0, false, &raw mut info));
            assert_eq!(info.channel_count, 2);
            assert_ne!(info.flags & CLAP_AUDIO_PORT_IS_MAIN, 0);
            assert_eq!(read_c(info.port_type), "stereo");
            assert_eq!(
                info.in_place_pair, CLAP_INVALID_ID,
                "in-place pairs would alias a live & and &mut over one buffer"
            );
            assert_eq!(read_c(info.name.as_ptr()), "Output");

            // The two directions are separate lists, and asking for one must never answer
            // with the other's bus.
            assert!((ports.get)(plugin, 0, true, &raw mut info));
            assert_eq!(read_c(info.name.as_ptr()), "Input");
            assert_eq!(info.channel_count, 2);

            assert!(!(ports.get)(plugin, 1, false, &raw mut info));
            assert!(!(ports.get)(plugin, 0, false, ptr::null_mut()));
            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn note_ports_advertise_the_clap_dialect_first() {
        use crate::abi::{
            CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI,
            CLAP_NOTE_DIALECT_MIDI2, ClapNotePortInfo, ClapPluginNotePorts,
        };
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let ports: &ClapPluginNotePorts = ext(plugin, CLAP_EXT_NOTE_PORTS);
            assert_eq!((ports.count)(plugin, true), 1);
            assert_eq!((ports.count)(plugin, false), 1);

            let mut info: ClapNotePortInfo = core::mem::zeroed();
            assert!((ports.get)(plugin, 0, true, &raw mut info));
            assert_eq!(
                info.supported_dialects,
                CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI,
                "a port that never advertised MIDI 2.0 must not claim it"
            );
            assert_eq!(info.supported_dialects & CLAP_NOTE_DIALECT_MIDI2, 0);
            assert_eq!(info.preferred_dialect, CLAP_NOTE_DIALECT_CLAP);
            assert_eq!(read_c(info.name.as_ptr()), "Event In");

            assert!(!(ports.get)(plugin, 9, true, &raw mut info));
            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn parameters_are_served_in_plain_units_and_round_trip_through_text() {
        use crate::abi::{
            CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, ClapParamInfo, ClapPluginParams,
        };
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let params: &ClapPluginParams = ext(plugin, CLAP_EXT_PARAMS);
            assert_eq!((params.count)(plugin), 1);

            let mut info: ClapParamInfo = core::mem::zeroed();
            assert!((params.get_info)(plugin, 0, &raw mut info));
            assert_eq!(info.id, 1);
            assert_eq!(read_c(info.name.as_ptr()), "Gain");
            assert_eq!(
                (info.min_value, info.max_value, info.default_value),
                (-60.0, 12.0, 0.0)
            );
            assert_ne!(info.flags & CLAP_PARAM_IS_AUTOMATABLE, 0);
            assert!(!(params.get_info)(plugin, 1, &raw mut info));
            assert!(!(params.get_info)(plugin, 0, ptr::null_mut()));

            let mut value = f64::NAN;
            assert!((params.get_value)(plugin, 1, &raw mut value));
            assert_eq!(value, 0.0, "plain units, never normalised");
            assert!(
                !(params.get_value)(plugin, 99, &raw mut value),
                "an unknown parameter id must be refused"
            );
            assert!(!(params.get_value)(plugin, 1, ptr::null_mut()));

            let mut parsed = f64::NAN;
            assert!((params.text_to_value)(
                plugin,
                1,
                c"-6 dB".as_ptr(),
                &raw mut parsed
            ));
            assert_eq!(parsed, -6.0);
            assert!(!(params.text_to_value)(
                plugin,
                1,
                c"nonsense".as_ptr(),
                &raw mut parsed
            ));
            assert!(!(params.text_to_value)(
                plugin,
                1,
                ptr::null(),
                &raw mut parsed
            ));
            assert!(!(params.text_to_value)(
                plugin,
                99,
                c"0".as_ptr(),
                &raw mut parsed
            ));

            let mut text = [0i8; 64];
            assert!((params.value_to_text)(
                plugin,
                1,
                -6.0,
                text.as_mut_ptr(),
                64
            ));
            assert_eq!(read_c(text.as_ptr()), "-6.00 dB");
            assert!(
                !(params.value_to_text)(plugin, 1, -6.0, ptr::null_mut(), 64),
                "a null buffer must be refused, not written to"
            );
            // A capacity smaller than the text truncates rather than failing, so a host
            // with a tiny buffer still gets something to show.
            let mut tiny = [0i8; 4];
            assert!((params.value_to_text)(
                plugin,
                1,
                -6.0,
                tiny.as_mut_ptr(),
                4
            ));
            assert_eq!(read_c(tiny.as_ptr()), "-6.");

            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn flush_applies_parameter_changes_that_arrive_outside_process() {
        use crate::abi::{CLAP_EXT_PARAMS, ClapPluginParams};
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let params: &ClapPluginParams = ext(plugin, CLAP_EXT_PARAMS);

            let events = EventList::with_param_values(&[(1, -12.0), (99, 3.0)]);
            (params.flush)(plugin, events.as_ptr(), ptr::null());

            let mut value = f64::NAN;
            assert!((params.get_value)(plugin, 1, &raw mut value));
            assert_eq!(value, -12.0);

            // A flush with no events at all, and one with a null list, must both be no-ops
            // rather than crashes.
            (params.flush)(plugin, ptr::null(), ptr::null());
            assert!((params.get_value)(plugin, 1, &raw mut value));
            assert_eq!(value, -12.0);
            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn latency_tail_and_render_report_what_the_plugin_declares() {
        use crate::abi::{
            CLAP_EXT_LATENCY, CLAP_EXT_RENDER, CLAP_EXT_TAIL, CLAP_RENDER_OFFLINE,
            CLAP_RENDER_REALTIME, ClapPluginLatency, ClapPluginRender, ClapPluginTail,
        };
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let latency: &ClapPluginLatency = ext(plugin, CLAP_EXT_LATENCY);
            let tail: &ClapPluginTail = ext(plugin, CLAP_EXT_TAIL);
            let render: &ClapPluginRender = ext(plugin, CLAP_EXT_RENDER);

            assert_eq!((latency.get)(plugin), 0);
            assert_eq!((tail.get)(plugin), 0);
            assert!(!(render.has_hard_realtime_requirement)(plugin));
            assert!((render.set)(plugin, CLAP_RENDER_OFFLINE));
            assert!((render.set)(plugin, CLAP_RENDER_REALTIME));
            assert!(
                !(render.set)(plugin, 42),
                "an unknown render mode must be refused, not silently treated as realtime"
            );
            ((*plugin).destroy)(plugin);
        }
    }

    // ---- state ------------------------------------------------------------------------

    /// Saves, mutates, reloads, and reports what the parameter ended up at.
    ///
    /// # Safety
    ///
    /// `plugin` must be a live, initialised instance.
    unsafe fn save_and_reload(plugin: *const ClapPlugin, stream: &TestStream, poke: f64) -> f64 {
        use crate::abi::{CLAP_EXT_PARAMS, CLAP_EXT_STATE, ClapPluginParams, ClapPluginState};
        // SAFETY: the caller guarantees `plugin` is live; the streams outlive the calls.
        unsafe {
            let state: &ClapPluginState = ext(plugin, CLAP_EXT_STATE);
            let params: &ClapPluginParams = ext(plugin, CLAP_EXT_PARAMS);
            assert!((state.save)(plugin, stream.ostream()));

            let mut parsed = 0.0;
            assert!((params.text_to_value)(
                plugin,
                1,
                c"3".as_ptr(),
                &raw mut parsed
            ));
            (params.flush)(
                plugin,
                EventList::with_param_values(&[(1, poke)]).as_ptr(),
                ptr::null(),
            );
            let mut value = f64::NAN;
            assert!((params.get_value)(plugin, 1, &raw mut value));
            assert_eq!(value, poke, "the poke must have taken effect");

            stream.rewind();
            assert!((state.load)(plugin, stream.istream()));
            assert!((params.get_value)(plugin, 1, &raw mut value));
            value
        }
    }

    #[test]
    fn state_round_trips_parameter_values_through_a_host_stream() {
        let host = TestHost::new();
        let stream = TestStream::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            assert_eq!(save_and_reload(plugin, &stream, -30.0), 0.0);
            assert!(!stream.data().is_empty());
            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn a_stream_that_moves_a_few_bytes_at_a_time_still_transfers_the_whole_blob() {
        let host = TestHost::new();
        // Seven bytes per call: enough to need dozens of round trips for a real blob, and
        // exactly the shape of a host that writes into a network socket.
        let stream = TestStream::with(Vec::new(), 7, false);
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            assert_eq!(save_and_reload(plugin, &stream, -18.0), 0.0);
            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn a_failing_stream_makes_save_and_load_fail_rather_than_half_succeed() {
        use crate::abi::{CLAP_EXT_STATE, ClapPluginState};
        let host = TestHost::new();
        let stream = TestStream::with(Vec::new(), 0, true);
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let state: &ClapPluginState = ext(plugin, CLAP_EXT_STATE);
            assert!(!(state.save)(plugin, stream.ostream()));
            assert!(!(state.load)(plugin, stream.istream()));
            assert!(!(state.save)(plugin, ptr::null()));
            assert!(!(state.load)(plugin, ptr::null()));
            ((*plugin).destroy)(plugin);
        }
    }

    #[test]
    fn a_corrupt_or_truncated_blob_is_rejected_instead_of_panicking() {
        use crate::abi::{CLAP_EXT_STATE, ClapPluginState};
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let state: &ClapPluginState = ext(plugin, CLAP_EXT_STATE);

            // First produce a real blob, then feed back mutilated versions of it.
            let good = TestStream::new();
            assert!((state.save)(plugin, good.ostream()));
            let blob = good.data();
            assert!(blob.len() > 8);

            for bad in [
                Vec::new(),
                blob[..blob.len() / 2].to_vec(),
                blob[..8].to_vec(),
                vec![0xff; 64],
                b"DAUXST\0\0garbage".to_vec(),
            ] {
                let stream = TestStream::with(bad, 0, false);
                assert!(
                    !(state.load)(plugin, stream.istream()),
                    "a malformed blob must be refused"
                );
            }

            // …and the instance is still usable afterwards, not poisoned by bad input.
            let fresh = TestStream::with(blob, 0, false);
            assert!((state.load)(plugin, fresh.istream()));
            ((*plugin).destroy)(plugin);
        }
    }

    // ---- audio ------------------------------------------------------------------------

    #[test]
    fn audio_reaches_the_plugin_and_comes_back_changed() {
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let p = &*plugin;
            assert!((p.activate)(plugin, 44_100.0, 0, 256));
            assert!((p.start_processing)(plugin));

            let mut block = host.block(1, 1, 128);
            assert_eq!(block.frames(), 128);
            block.fill_input(0, 0, 0.5);
            block.fill_input(0, 1, -0.25);
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_CONTINUE);
            assert_eq!(block.output(0, 0), &[0.5f32; 128][..]);
            assert_eq!(block.output(0, 1), &[-0.25f32; 128][..]);

            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_block_with_no_buses_at_all_is_processed_rather_than_refused() {
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let p = &*plugin;
            assert!((p.activate)(plugin, 48_000.0, 0, 64));
            assert!((p.start_processing)(plugin));
            // A MIDI effect gets exactly this: frames, events, and no audio.
            let mut block = host.block(0, 0, 32);
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_CONTINUE);
            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn reset_is_accepted_while_processing_even_though_abi_v1_would_refuse_it() {
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let p = &*plugin;
            assert!((p.activate)(plugin, 48_000.0, 0, 64));
            assert!((p.start_processing)(plugin));
            // CLAP allows this; a host locates the playhead mid-playback and expects delay
            // lines to clear. If the adapter simply forwarded it, `PluginInstance` would
            // refuse and the tail of the previous position would bleed through.
            (p.reset)(plugin);
            let mut block = host.block(1, 1, 16);
            assert_eq!(
                (p.process)(plugin, block.as_ptr()),
                CLAP_PROCESS_CONTINUE,
                "the instance must still be processing after a mid-run reset"
            );
            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_reactivation_at_a_new_sample_rate_is_accepted() {
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block.
        unsafe {
            let plugin = instance(&host);
            let p = &*plugin;
            assert!((p.activate)(plugin, 44_100.0, 0, 64));
            assert!((p.start_processing)(plugin));
            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            assert!((p.activate)(plugin, 96_000.0, 32, 512));
            assert!((p.start_processing)(plugin));
            let mut block = host.block(1, 1, 512);
            assert_eq!((p.process)(plugin, block.as_ptr()), CLAP_PROCESS_CONTINUE);
            (p.stop_processing)(plugin);
            (p.deactivate)(plugin);
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn destroying_an_instance_the_host_never_stopped_is_survivable() {
        let host = TestHost::new();
        // SAFETY: the instance is created and destroyed inside this block; skipping
        // `stop_processing` and `deactivate` is exactly the host bug under test.
        unsafe {
            let plugin = instance(&host);
            let p = &*plugin;
            assert!((p.activate)(plugin, 48_000.0, 0, 64));
            assert!((p.start_processing)(plugin));
            (p.destroy)(plugin);
        }
    }

    #[test]
    fn a_null_host_is_refused_rather_than_dereferenced() {
        let factory = open::<TestFactory>();
        // SAFETY: `factory` is live; the null host is the input under test.
        unsafe {
            let plugin =
                (factory.create_plugin)(factory, ptr::null(), c"com.example.clap-test".as_ptr());
            assert!(plugin.is_null());
        }
    }
}
