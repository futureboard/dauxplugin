//! `export_entry!` has to work from outside the crate that defines it, expanding to a real
//! `#[unsafe(no_mangle)] pub static clap_entry`.
//!
//! That is not something a unit test inside `daux-format-clap` can check: a second
//! `clap_entry` symbol in the library's own test binary would collide with anything else
//! that exported one, and the macro's `$crate` paths only resolve properly when it is
//! invoked from another crate. This integration test is therefore the only place the macro
//! is actually expanded, and it doubles as the smallest possible example of exporting a
//! plug-in.

use daux_format_clap::abi::{ClapPluginEntry, ClapPluginFactory, ClapVersion};
use daux_format_clap::daux_plugin_api::{
    AudioBuses, BusLayout, Capabilities, DauxController, DauxError, DauxFactory, DauxPlugin,
    DauxProcessor, DauxResult, ErrorKind, Param, ParamId, Params, PluginDescriptor, ProcessConfig,
    ProcessContext, ProcessEvents, ProcessStatus, StateReader, StateWriter,
};

/// The permanent id of the plug-in this test exports.
const PLUGIN_ID: &str = "com.example.clap-export";

/// A plug-in with no parameters that silences whatever it is given.
#[derive(Default)]
struct Silence;

impl DauxProcessor for Silence {
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

impl Params for Silence {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)> {
        Vec::new()
    }
}

impl DauxController for Silence {
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

impl DauxPlugin for Silence {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder(PLUGIN_ID, "Silence")
            .vendor("Example Audio")
            .capabilities(Capabilities::AUDIO_EFFECT)
            .build()
            .expect("the exported descriptor is valid")
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

/// The factory the entry point is built from.
#[derive(Default)]
struct ExportedFactory;

impl DauxFactory for ExportedFactory {
    fn plugin_count(&self) -> usize {
        1
    }

    fn descriptor(&self, index: usize) -> Option<PluginDescriptor> {
        (index == 0).then(Silence::descriptor)
    }

    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>> {
        if id == PLUGIN_ID {
            Ok(Box::new(Silence))
        } else {
            Err(DauxError::new(ErrorKind::NotFound, "no such plug-in"))
        }
    }
}

// The line a plug-in author writes. Everything below tests what it produced.
daux_format_clap::export_entry!(ExportedFactory);

#[test]
fn the_exported_symbol_is_a_usable_clap_entry() {
    // Reading the exported `static` by name is what a host's `dlsym` does, one step removed.
    let entry: &ClapPluginEntry = &clap_entry;
    assert_eq!(entry.clap_version, ClapVersion::CURRENT);
    assert!(entry.clap_version.is_compatible());

    // SAFETY: `entry` is the `'static` table the macro emitted; `init` tolerates a null
    // path, and `get_factory` returns either null or a `'static` factory table.
    unsafe {
        assert!((entry.init)(c"/tmp/Silence.clap".as_ptr()));

        let raw = (entry.get_factory)(c"clap.plugin-factory".as_ptr());
        assert!(!raw.is_null(), "the plug-in factory must be published");
        let factory = &*raw.cast::<ClapPluginFactory>();

        assert_eq!((factory.get_plugin_count)(factory), 1);
        let descriptor = (factory.get_plugin_descriptor)(factory, 0);
        assert!(!descriptor.is_null());
        let id = core::ffi::CStr::from_ptr((*descriptor).id);
        assert_eq!(id.to_str().expect("the id is UTF-8"), PLUGIN_ID);

        assert!(
            (entry.get_factory)(c"clap.preset-discovery-factory".as_ptr()).is_null(),
            "a factory this binary does not export must answer null"
        );

        (entry.deinit)();
    }
}

#[test]
fn an_instance_created_through_the_exported_entry_runs_a_block() {
    // SAFETY: every pointer below comes from the exported entry or from the local buffers,
    // and the calls follow CLAP's prescribed order.
    unsafe {
        let raw = (clap_entry.get_factory)(c"clap.plugin-factory".as_ptr());
        let factory = &*raw.cast::<ClapPluginFactory>();

        // A host that offers nothing at all: no extensions, no callbacks.
        let host = daux_format_clap::abi::ClapHost {
            clap_version: ClapVersion::CURRENT,
            host_data: core::ptr::null_mut(),
            name: c"Bare Host".as_ptr(),
            vendor: c"".as_ptr(),
            url: c"".as_ptr(),
            version: c"".as_ptr(),
            get_extension: None,
            request_restart: None,
            request_process: None,
            request_callback: None,
        };
        let plugin = (factory.create_plugin)(
            factory,
            core::ptr::from_ref(&host),
            c"com.example.clap-export".as_ptr(),
        );
        assert!(
            !plugin.is_null(),
            "a host with no extensions must still work"
        );
        let p = &*plugin;

        assert!((p.init)(plugin));
        assert!((p.activate)(plugin, 48_000.0, 0, 64));
        assert!((p.start_processing)(plugin));

        let frames = 32usize;
        let mut left = vec![0.5f32; frames];
        let mut right = vec![-0.5f32; frames];
        let mut channels = [left.as_mut_ptr(), right.as_mut_ptr()];
        let mut output = daux_format_clap::abi::ClapAudioBuffer {
            data32: channels.as_mut_ptr(),
            data64: core::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let process = daux_format_clap::abi::ClapProcess {
            steady_time: -1,
            frames_count: frames as u32,
            transport: core::ptr::null(),
            audio_inputs: core::ptr::null(),
            audio_outputs: core::ptr::from_mut(&mut output),
            audio_inputs_count: 0,
            audio_outputs_count: 1,
            in_events: core::ptr::null(),
            out_events: core::ptr::null(),
        };
        let status = (p.process)(plugin, core::ptr::from_ref(&process));
        assert_eq!(
            status,
            daux_format_clap::abi::CLAP_PROCESS_CONTINUE_IF_NOT_QUIET
        );

        (p.stop_processing)(plugin);
        (p.deactivate)(plugin);
        (p.destroy)(plugin);

        assert_eq!(
            left,
            vec![0.0f32; frames],
            "the plug-in silences its output"
        );
        assert_eq!(right, vec![0.0f32; frames]);
    }
}

#[test]
fn the_capability_report_is_reachable_from_outside_the_crate() {
    let report = daux_format_clap::compatibility_report(&Silence::descriptor());
    assert!(
        report.is_empty(),
        "a plain stereo effect maps onto CLAP without loss: {report:?}"
    );
}
