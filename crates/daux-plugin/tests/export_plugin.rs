//! `export_plugin!` — one line, every enabled format, from a crate that depends only on
//! `daux-plugin`.
//!
//! The property under test is not "the macro compiles". It is that a crate whose manifest
//! names `daux-plugin` and nothing else ends up with the exported symbols of `daux-format-axt`,
//! `daux-format-vst3` and `daux-format-clap` in its binary, wired to *its* factory. That is
//! the whole reason the facade exists, and it can only be checked from outside: this test
//! crate has no dev-dependencies, so every path below is one a plug-in author can write.
//!
//! Each format is then driven the way its host drives it — read the module header, ask the
//! factory how many plug-ins there are, release it — because a symbol that exists but answers
//! wrongly is worse than one that is missing.
//!
//! The file is empty when no format feature is enabled, which is also the one case in which
//! [`daux_plugin::export_plugin!`] refuses to compile at all.

#![cfg(any(feature = "axt", feature = "vst3", feature = "clap"))]

use daux_plugin::prelude::*;

// ------------------------------------------------------------------ the plug-ins ---

/// The first plug-in of the exported module.
#[derive(Default)]
struct Alpha;

/// The second, so that the exported factory is identifiable by more than the fact that it
/// exists: a module reporting one plug-in could be anybody's.
#[derive(Default)]
struct Beta;

/// The shared body of both plug-ins; only the descriptor differs.
macro_rules! trivial_plugin {
    ($ty:ty, $id:literal, $name:literal) => {
        impl DauxProcessor for $ty {
            fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
                config.validate()
            }

            fn process<'a>(
                &mut self,
                _ctx: &ProcessContext<'a>,
                audio: &mut AudioBuses<'a, f32>,
                _events: &mut ProcessEvents<'a>,
            ) -> ProcessStatus {
                audio.silence_outputs();
                ProcessStatus::ContinueIfNotQuiet
            }
        }

        impl Params for $ty {
            fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
                Vec::new()
            }
        }

        impl DauxController for $ty {
            fn params(&self) -> &dyn Params {
                self
            }

            fn save_state(&self, _w: &mut StateWriter) -> DauxResult<()> {
                Ok(())
            }

            fn load_state(&mut self, _r: &StateReader) -> DauxResult<()> {
                Ok(())
            }
        }

        impl DauxPlugin for $ty {
            fn descriptor() -> PluginDescriptor {
                PluginDescriptor::builder($id, $name)
                    .vendor("DAUxPlug tests")
                    .category(Category::Effect)
                    .capabilities(Capabilities::AUDIO_EFFECT)
                    .version(Version::new(1, 0, 0))
                    .build()
                    .expect("valid")
            }

            fn bus_layout(&self) -> BusLayout {
                BusLayout::stereo_effect()
            }

            fn processor(&mut self) -> &mut dyn DauxProcessor {
                self
            }

            fn controller(&mut self) -> &mut dyn DauxController {
                self
            }
        }
    };
}

trivial_plugin!(Alpha, "com.example.exported.alpha", "Exported Alpha");
trivial_plugin!(Beta, "com.example.exported.beta", "Exported Beta");

/// The documented way to ship several plug-ins in one binary: a `Default` newtype around a
/// [`PluginRegistry`]. Every format's entry point constructs the factory with no arguments,
/// so `Default` is not a convenience — it is the whole interface a module export has.
struct TwoPlugins(PluginRegistry);

impl Default for TwoPlugins {
    fn default() -> Self {
        let mut registry = PluginRegistry::new();
        registry.register::<Alpha>().register::<Beta>();
        Self(registry)
    }
}

impl DauxFactory for TwoPlugins {
    fn plugin_count(&self) -> usize {
        self.0.plugin_count()
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        self.0.descriptor(index)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        self.0.create(id)
    }
}

// The line a plug-in writes. Everything below tests what it produced.
daux_plugin::export_plugin!(TwoPlugins);

/// How many plug-ins every format must report, whichever way it is asked.
const EXPECTED_PLUGINS: usize = 2;

#[test]
fn the_factory_the_macro_was_given_is_the_one_this_crate_defined() {
    let factory = TwoPlugins::default();
    assert_eq!(factory.plugin_count(), EXPECTED_PLUGINS);
    assert!(factory.contains("com.example.exported.alpha"));
    assert!(factory.contains("com.example.exported.beta"));
}

// -------------------------------------------------------------------------- AXT ---

#[cfg(feature = "axt")]
mod axt {
    use core::ptr;

    use daux_plugin::formats::axt::__private::{DauxFactoryV1, DauxPluginEntryV1};

    // SAFETY: `export_plugin!` defined this symbol in this very binary, with exactly this
    // signature — `unsafe extern "C" fn() -> *const DauxPluginEntryV1`, the ABI's
    // `daux_plugin_entry_v1` (abi-v1 §4). Declaring it here is how a host reaches it after
    // `GetProcAddress`/`dlsym`; the linker resolves it to the definition above. Nothing is
    // mutated and the returned pointer is to a `static`.
    unsafe extern "C" {
        fn daux_plugin_entry_v1() -> *const DauxPluginEntryV1;
    }

    /// Borrows the module header the exported symbol points at.
    fn header() -> &'static DauxPluginEntryV1 {
        // SAFETY: the symbol is defined in this binary by `export_plugin!` and returns a
        // pointer to an immortal `static`, so the reference is valid for `'static` and can
        // never dangle. The call itself only reads that address.
        unsafe {
            let raw = daux_plugin_entry_v1();
            assert!(!raw.is_null(), "the entry point must never return null");
            &*raw
        }
    }

    #[test]
    fn the_entry_symbol_exists_and_carries_a_valid_module_header() {
        let entry = header();
        // `DAUX_ABI_MAGIC`, spelled out rather than imported: a host validates the literal
        // bytes it read from a file, so pinning them here is the point.
        assert_eq!(entry.magic, 0x4441_5558_4142_4931);
        assert_eq!(entry.abi_version_major, 1);
        assert_eq!(entry.size, DauxPluginEntryV1::SIZE);
        assert!(entry.is_v1_0_compatible());
        assert_eq!(entry._pad0, 0, "reserved fields are zero (abi-v1 §3)");
        assert!(entry.reserved.iter().all(|word| *word == 0));
        assert_eq!(entry.sdk_name.as_str(), daux_plugin::formats::axt::SDK_NAME);
    }

    #[test]
    fn the_header_hands_out_this_crates_factory() {
        let entry = header();
        let mut factory = DauxFactoryV1::null();

        // SAFETY: `create_factory` is this module's own function, reached through the header
        // it published. A null `host` is explicitly permitted (abi-v1 §4) and `out_factory`
        // points at a live, writable, correctly aligned local.
        let status = unsafe { (entry.create_factory)(ptr::null(), &raw mut factory) };
        assert!(status.is_ok(), "create_factory failed: {}", status.as_i32());
        assert!(!factory.is_null(), "a successful call must fill the pair");

        // SAFETY: the table was produced by the call above, is owned by this module, is
        // immutable for its whole lifetime and outlives the borrow, which ends before
        // `destroy_factory`.
        let api = unsafe { factory.api() }.expect("a non-null pair carries a table");
        // SAFETY: `plugin_count` is `[any-thread]` and takes the handle it was just given.
        let count = unsafe { (api.plugin_count)(factory.handle) };
        assert_eq!(
            count as usize,
            super::EXPECTED_PLUGINS,
            "the exported factory must be `TwoPlugins`, not some default"
        );

        // SAFETY: the pair is the one `create_factory` produced and no instance was created
        // from it, which is what abi-v1 §4 requires before destroying a factory.
        unsafe { (entry.destroy_factory)(factory) };
    }
}

// ------------------------------------------------------------------------- VST3 ---

#[cfg(feature = "vst3")]
mod vst3 {
    use core::ffi::c_void;

    use daux_plugin::formats::vst3::api::IPluginFactoryVtbl;

    // The macro emits these into this module with `#[unsafe(no_mangle)]`; naming them from
    // Rust is how the test proves they are the functions a host will find.
    use super::{ExitDll, GetPluginFactory, InitDll};

    #[test]
    fn the_module_hooks_are_exported_and_answer_yes() {
        assert!(
            InitDll(),
            "a host that cannot initialise the module gives up"
        );
        assert!(ExitDll());
        // Hosts call them more than once; neither may become false.
        assert!(InitDll());
        assert!(ExitDll());
    }

    #[test]
    fn get_plugin_factory_hands_out_this_crates_two_classes() {
        let raw: *mut c_void = GetPluginFactory();
        assert!(
            !raw.is_null(),
            "a null factory is how a host learns the module exports nothing"
        );

        // SAFETY: a VST3 object is a `#[repr(C)]` struct whose first field is a pointer to its
        // vtable, which is what COM requires and what `daux-format-vst3` builds. `raw` was
        // produced by this binary's own `GetPluginFactory` and carries one reference this test
        // owns until it calls `release`.
        let count = unsafe {
            let vtbl = *raw.cast::<*const IPluginFactoryVtbl>();
            assert!(!vtbl.is_null());
            ((*vtbl).count_classes)(raw)
        };
        assert_eq!(
            count as usize,
            super::EXPECTED_PLUGINS,
            "the exported factory must be `TwoPlugins`, not some default"
        );

        // SAFETY: same object, same vtable; this drops the one reference the call above
        // handed us, which is the contract of `GetPluginFactory`. Nothing touches `raw` after.
        let remaining = unsafe {
            let vtbl = *raw.cast::<*const IPluginFactoryVtbl>();
            ((*vtbl).release)(raw)
        };
        assert_eq!(
            remaining, 0,
            "the host holds the only reference, so releasing it must free the factory"
        );
    }
}

// ------------------------------------------------------------------------- CLAP ---

#[cfg(feature = "clap")]
mod clap {
    use daux_plugin::formats::clap::abi::{CLAP_PLUGIN_FACTORY_ID, ClapPluginFactory};

    // The macro emits the `clap_entry` static into this module.
    use super::clap_entry;

    #[test]
    fn the_entry_static_declares_a_version_a_host_will_load() {
        assert!(
            clap_entry.clap_version.is_compatible(),
            "an incompatible version makes every CLAP host skip the module"
        );
        assert_eq!(clap_entry.clap_version.major, 1);
    }

    #[test]
    fn the_entry_publishes_this_crates_factory_and_nothing_else() {
        // SAFETY: every function below belongs to `daux-format-clap` and is reached through
        // the static this binary exports. `init` tolerates any C string, `get_factory`
        // tolerates a null id, and the returned table is a `'static` owned by the module.
        // The sequence — init, use, deinit — is the one CLAP mandates.
        unsafe {
            assert!((clap_entry.init)(c"/tmp/Exported.clap".as_ptr()));

            let unknown = (clap_entry.get_factory)(c"clap.preset-discovery-factory".as_ptr());
            assert!(
                unknown.is_null(),
                "a factory id this adapter does not implement must be refused, not guessed"
            );
            assert!((clap_entry.get_factory)(core::ptr::null()).is_null());

            let raw = (clap_entry.get_factory)(CLAP_PLUGIN_FACTORY_ID.as_ptr());
            assert!(!raw.is_null(), "the plug-in factory must be published");
            let factory = &*raw.cast::<ClapPluginFactory>();

            assert_eq!(
                (factory.get_plugin_count)(factory) as usize,
                super::EXPECTED_PLUGINS,
                "the exported factory must be `TwoPlugins`, not some default"
            );

            let first = (factory.get_plugin_descriptor)(factory, 0);
            assert!(!first.is_null());
            let id = core::ffi::CStr::from_ptr((*first).id)
                .to_str()
                .expect("the adapter writes UTF-8 ids");
            assert_eq!(id, "com.example.exported.alpha");

            assert!(
                (factory.get_plugin_descriptor)(factory, u32::MAX).is_null(),
                "an out-of-range index must be refused rather than indexed"
            );

            (clap_entry.deinit)();
        }
    }
}
