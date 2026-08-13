//! Gain effect with an egui editor.
//!
//! [`examples/gain`] with a face on it. The DSP is deliberately the same shape, so the diff
//! between the two files is exactly "what an editor costs".
//!
//! # What it shows
//!
//! | Topic | Where |
//! |---|---|
//! | one parameter bank shared by the processor, the controller and the editor | [`Gain::params`] |
//! | gesture-correct controls via [`ParamBinding`] | [`draw`] |
//! | an editor whose lifetime is independent of the DSP's | [`Gain::create_editor`] |
//! | audio → UI data with no lock anywhere | [`GainParams::output`] |
//! | the host's automation service, and working without one | [`Gain::set_host`] |
//!
//! # No `Mutex` between the audio thread and the editor
//!
//! The editor and `process` share exactly one thing: the parameter bank, held in an
//! [`Arc<GainParams>`]. Every value inside it lives in a [`daux_rt::AtomicF64`] or
//! [`daux_rt::AtomicF32`], so a knob turned on the UI thread is visible to the next audio
//! block without a lock, and the output meter written by the audio thread is readable by the
//! editor the same way. A `Mutex` here would be a real-time bug even while uncontended,
//! because the UI thread can be preempted while holding it.
//!
//! # Gestures, and why they are not optional
//!
//! A host records automation between `gesture_begin` and `gesture_end`. Getting that wrong is
//! invisible from the outside and produces either no automation at all or a lane latched in
//! write mode forever. [`ParamBinding`] owns that state machine — it refuses a second begin,
//! ignores an end with no begin, tells the host the value the parameter *actually took* after
//! clamping, and closes an open gesture from its own `Drop` if the editor is destroyed
//! mid-drag. Every widget in `daux-graphics-egui` drives one, which is why this file contains
//! no gesture code at all.
//!
//! # The painter
//!
//! egui produces shapes, not pixels; a painter rasterises them. This example uses
//! [`HeadlessPainter`], which runs the complete egui frame — layout, interaction,
//! tessellation — and discards the triangles, so the example builds and its tests run with no
//! GPU. Point [`Painter`] at `daux-graphics-wgpu` (or `daux-graphics-gl`) and the same editor
//! draws on screen; nothing else in this file changes.
//!
//! [`examples/gain`]: https://github.com/futureboard-studio/dauxplug/tree/main/examples/gain
//! [`Arc<GainParams>`]: std::sync::Arc

use std::sync::Arc;

use daux_plugin::dsp::db_to_gain;
use daux_plugin::graphics::egui::egui;
use daux_plugin::graphics::egui::{
    EguiEditor, HeadlessPainter, ParamKnob, ParamToggle, ParamValueEdit,
};
use daux_plugin::prelude::*;
use daux_plugin::{HostParams, ParamBinding};

/// The permanent parameter ids.
mod param_id {
    /// Output gain in decibels.
    pub const GAIN: u32 = 1;
    /// Bypass switch.
    pub const BYPASS: u32 = 2;
    /// Output level meter, written by the audio thread and read by the editor.
    pub const OUTPUT: u32 = 3;
}

/// The state schema version [`Gain::save_state`] writes.
const STATE_VERSION: u32 = 1;

/// The editor's preferred size in logical pixels.
const EDITOR_SIZE: LogicalSize = LogicalSize {
    width: 320.0,
    height: 180.0,
};

/// Which painter this build uses.
///
/// [`HeadlessPainter`] runs the whole egui frame and throws the triangles away, so the example
/// needs no GPU and its tests run in CI. Swap this one line for
/// `daux_plugin::graphics::wgpu::WgpuPainter` (feature `wgpu`) or the OpenGL painter to put
/// the same editor on screen.
type Painter = HeadlessPainter;

/// The editor type this plug-in produces.
pub type GainEditor = EguiEditor<Painter>;

/// The parameters, shared by the processor, the controller and the editor.
#[derive(DauxParams)]
pub struct GainParams {
    /// Output gain in decibels.
    #[param(id = param_id::GAIN, name = "Gain", range = -60.0..=12.0, default = 0.0,
            unit = "dB", decimals = 1, smoothing = "exponential(15.0)",
            flags(automatable, modulatable))]
    pub gain: FloatParam,

    /// Passes the input through untouched when on.
    #[param(id = param_id::BYPASS, name = "Bypass", default = false,
            labels("Active", "Bypassed"), flags(automatable, bypass))]
    pub bypass: BoolParam,

    /// Peak output level of the last block, in dBFS.
    ///
    /// Written by the audio thread and read by the editor. A [`MeterParam`] stores its value
    /// in an atomic, so this crosses the thread boundary without a lock and without a queue —
    /// which is the whole reason a meter is modelled as a read-only parameter rather than as
    /// a side channel.
    #[param(id = param_id::OUTPUT, name = "Output", range = -60.0..=12.0, unit = "dB",
            decimals = 1, flags(is_meter, read_only))]
    pub output: MeterParam,
}

/// A stereo gain with an editor.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.gainegui",
    name = "DAUx Gain (egui)",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Stereo gain with an egui editor.",
    license = "MIT OR Apache-2.0",
    category = "effect",
    capabilities(
        audio_effect,
        has_gui,
        sample_accurate_auto,
        offline_render,
        sandbox_safe
    ),
    features("utility", "gain"),
    state_schema_version = STATE_VERSION
)]
pub struct Gain {
    /// The parameter bank. `Arc` because the editor holds a clone of it for as long as it is
    /// open, and the editor's lifetime is not the plug-in's.
    params: Arc<GainParams>,
    /// The host's services, or [`HostServices::null`] until the host offers any.
    ///
    /// Cloned into the editor so its controls can report gestures. Every service inside is an
    /// `Option`: a plug-in that only works when the host provides an automation service does
    /// not work in a preview harness, in `daux run`, or in a host that has not implemented it
    /// yet.
    host: HostServices,
    /// Ramps the gain over the block so an automation jump does not click.
    smoother: Smoother,
    /// One coefficient per sample of the largest block the host promised, sized in `prepare`.
    gain_ramp: Vec<f32>,
}

impl Default for Gain {
    /// `[main-thread]` A fresh instance at the parameter defaults.
    fn default() -> Self {
        let params = Arc::new(GainParams::new());
        let smoother = params.gain.smoother();
        Self {
            params,
            host: HostServices::null(),
            smoother,
            gain_ramp: Vec::new(),
        }
    }
}

impl Gain {
    /// `[main-thread]` The parameter bank, for an editor or a test.
    #[must_use]
    pub fn params(&self) -> &Arc<GainParams> {
        &self.params
    }
}

/// `[main-thread]` Draws one egui frame of the editor.
///
/// A free function rather than a closure body so that a test can call it directly, and so the
/// interesting part — how a control is bound to a parameter — is not buried in a `move ||`.
///
/// `host` is `None` in a preview harness or a unit test. Everything still works; the value
/// changes simply go nowhere but the parameters themselves.
pub fn draw(ui: &mut egui::Ui, params: &GainParams, host: Option<&dyn HostParams>) {
    ui.heading("DAUx Gain");
    ui.separator();

    ui.horizontal(|ui| {
        // One binding per control per frame. The binding is what the widget drives: it owns
        // the gesture state machine, so nothing in this function has to know that a host
        // records automation between a begin and an end.
        let gain = ParamBinding::new(&params.gain, host);
        ui.add(ParamKnob::new(&gain).diameter(64.0));

        ui.vertical(|ui| {
            ui.label("Gain");
            // Text entry is a complete edit rather than a drag, and the binding brackets it in
            // a gesture of its own so the host records one automation point rather than none.
            ui.add(ParamValueEdit::new(&gain).width(84.0));
        });
    });

    let bypass = ParamBinding::new(&params.bypass, host);
    ui.add(ParamToggle::new(&bypass));

    ui.separator();
    // The meter. Reading it is one atomic load — no lock, no queue, no `try_recv`, and the
    // audio thread never learns that an editor exists.
    let level = params.output.value();
    ui.label(format!("Output {level:>6.1} dBFS"));
}

impl DauxProcessor for Gain {
    /// `[main-thread]` Sizes the ramp. The only place this plug-in allocates.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.gain_ramp.clear();
        self.gain_ramp.resize(config.max_block_size as usize, 1.0);
        self.smoother.prepare(config.sample_rate);
        self.smoother
            .reset_to(db_to_gain(self.params.gain.value_f32()));
        self.params.output.clear();
        Ok(())
    }

    /// `[audio-thread]` Drops the ramp's history and the meter's hold.
    fn reset(&mut self) {
        self.smoother
            .reset_to(db_to_gain(self.params.gain.value_f32()));
        self.params.output.clear();
    }

    /// `[audio-thread]` Copies input to output, applies the ramped gain, and publishes a peak.
    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let frames = ctx.frames().min(self.gain_ramp.len());
        let bypassed = self.params.bypass.value();

        self.smoother
            .set_target(db_to_gain(self.params.gain.value_f32()));

        // Automation applied at its own sample offset: fill the ramp up to the event, move
        // the target, carry on. See `examples/gain` for the same loop with more commentary.
        let mut filled = 0usize;
        for index in 0..events.input().len() {
            let Some(DauxEvent::ParamValue(event)) = events.input().get(index) else {
                continue;
            };
            let at = (event.header.time as usize).clamp(filled, frames);
            self.smoother.next_block(&mut self.gain_ramp[filled..at]);
            filled = at;

            if let Some(param) = self.params.param(ParamId::new(event.param_id)) {
                param.set_plain(event.value);
            }
            self.smoother
                .set_target(db_to_gain(self.params.gain.value_f32()));
        }
        self.smoother
            .next_block(&mut self.gain_ramp[filled..frames]);

        let input = audio.main_input();
        let Some(mut output) = audio.main_output() else {
            return ProcessStatus::Sleep;
        };
        if let Some(input) = input
            && output.copy_from(&input).is_err()
        {
            output.fill_silence();
            return ProcessStatus::Error;
        }

        let mut peak = 0.0f32;
        let ramp = &self.gain_ramp[..frames];
        for channel in output.split_channels_mut() {
            for (sample, gain) in channel.iter_mut().zip(ramp) {
                if !bypassed {
                    *sample *= gain;
                }
                peak = peak.max(sample.abs());
            }
        }

        // One atomic store, on the audio thread, with no idea whether anyone is reading it.
        // `push_peak` keeps the larger of the stored and the new value, so a meter sampled at
        // 60 Hz never misses a transient that happened between two repaints.
        self.params
            .output
            .push_peak(daux_plugin::dsp::gain_to_db(peak));

        ProcessStatus::ContinueIfNotQuiet
    }
}

impl DauxController for Gain {
    fn params(&self) -> &dyn Params {
        self.params.as_ref()
    }

    /// `[main-thread]` Keeps the host's services so the editor can report gestures.
    fn set_host(&mut self, host: HostServices) {
        self.host = host;
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_f64("gain_db", self.params.gain.value());
        w.put_bool("bypass", self.params.bypass.value());
        // The meter is not state: it describes the last block of audio, not the user's
        // intent, and restoring it would show a level that never happened.
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        if let Some(db) = r.opt_f64("gain_db") {
            self.params.gain.set_plain(db);
        }
        if let Some(bypass) = r.opt_bool("bypass") {
            self.params.bypass.set(bypass);
        }
        Ok(())
    }
}

impl DauxPlugin for Gain {
    fn descriptor() -> PluginDescriptor {
        Self::descriptor()
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

    /// `[main-thread]` Builds a fresh editor.
    ///
    /// May be called any number of times while audio is running, and the result may be dropped
    /// at any point: an editor's lifetime is independent of the processor's (`CLAUDE.md`
    /// rule 9). Nothing here touches DSP state, and nothing the editor owns is reachable from
    /// `process` — the shared parameter bank is atomic, and the audio thread never learns that
    /// an editor exists.
    fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
        // Clones of an `Arc` and of `HostServices` (itself a bundle of `Arc`s): the editor
        // owns what it reads, so dropping the plug-in's copy would not dangle.
        let params = Arc::clone(&self.params);
        let host = self.host.clone();
        editor(EguiEditor::new(Painter::new(), EDITOR_SIZE, move |ui| {
            draw(ui, &params, host.params());
        }))
    }

    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        let channels = |bus: Option<&BusInfo>| bus.map_or(0, BusInfo::channel_count);
        let inputs = channels(layout.main_input());
        let outputs = channels(layout.main_output());
        layout.inputs.len() <= 1
            && layout.outputs.len() == 1
            && matches!(outputs, 1 | 2)
            && (inputs == 0 || inputs == outputs)
    }
}

export_plugin!(SingleFactory<Gain>);

/// The allocation tripwire, installed only while this crate's tests are compiled.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_plugin::CountingAllocator = daux_plugin::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin::daux_rt::{AllocGuard, counting_allocator_installed};
    use daux_plugin::graphics::{
        DauxGraphic, GraphicContext, GraphicRenderer, InputEvent, LogicalPoint, Modifiers,
        PhysicalSize, PointerButton, ScaleFactor, WindowTarget,
    };
    use daux_plugin::{EventBuffer, HostInfo, RescanFlags, downcast_editor};
    use std::sync::Mutex;

    fn config() -> ProcessConfig {
        ProcessConfig::new(48_000.0, 256)
    }

    /// A host that records the automation calls an editor makes.
    #[derive(Default)]
    struct SpyHost {
        calls: Mutex<Vec<String>>,
    }

    impl SpyHost {
        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("no test panics while holding this")
                .clone()
        }

        fn push(&self, call: String) {
            self.calls
                .lock()
                .expect("no test panics while holding this")
                .push(call);
        }
    }

    impl HostParams for SpyHost {
        fn gesture_begin(&self, id: ParamId) {
            self.push(format!("begin {}", id.get()));
        }

        fn gesture_end(&self, id: ParamId) {
            self.push(format!("end {}", id.get()));
        }

        fn changed(&self, id: ParamId, plain: f64) {
            self.push(format!("changed {} {plain}", id.get()));
        }

        fn rescan(&self, _flags: RescanFlags) {
            self.push("rescan".to_owned());
        }
    }

    /// A plug-in with the spy installed as the host's automation service.
    fn with_spy() -> (Gain, Arc<SpyHost>) {
        let spy = Arc::new(SpyHost::default());
        let mut gain = Gain::default();
        gain.set_host(
            HostServices::builder()
                .info(HostInfo::new("Test", "DAUxPlug", "0.1"))
                .params(Arc::clone(&spy) as Arc<dyn HostParams>)
                .build(),
        );
        (gain, spy)
    }

    /// A window handle that is never dereferenced: [`HeadlessPainter`] only records the size.
    fn window() -> WindowTarget {
        WindowTarget::win32(1).expect("a non-null handle")
    }

    /// Opens `editor` against a fake host window.
    fn open(editor: &mut dyn DauxGraphic, host: &HostServices) {
        let profile = daux_plugin::graphics::egui::profile(GraphicRenderer::Software);
        let mut ctx = GraphicContext::new(
            window(),
            PhysicalSize::new(320, 180),
            ScaleFactor::ONE,
            profile,
            host,
        );
        editor
            .open(&mut ctx)
            .expect("the headless editor must open");
    }

    /// Runs one block of audio and returns the left output channel.
    fn run(gain: &mut Gain, frames: usize, level: f32) -> Vec<f32> {
        let mut input = AudioStorage::<f32>::new(2, frames);
        input.fill(level);
        let mut output = AudioStorage::<f32>::new(2, frames);
        let cfg = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(frames, &cfg, &host);
        {
            let inputs = [input.as_ref()];
            let mut outputs = [output.as_mut()];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
            let events = EventBuffer::with_capacity(1, 16);
            let mut sink = EventBuffer::with_capacity(1, 16);
            let mut ports = ProcessEvents::new(&events, &mut sink);
            assert_ne!(
                gain.process(&ctx, &mut buses, &mut ports),
                ProcessStatus::Error
            );
        }
        output.channel(0).expect("channel 0").to_vec()
    }

    // ---- DSP -------------------------------------------------------------------------------

    #[test]
    fn the_gain_actually_scales_the_signal() {
        for db in [-24.0, -6.0, 0.0, 12.0] {
            let mut gain = Gain::default();
            gain.params.gain.set_plain(db);
            gain.prepare(&config()).expect("a valid config");
            let out = run(&mut gain, 128, 1.0);
            let expected = db_to_gain(db as f32);
            assert!(
                (out[127] - expected).abs() < 1e-4,
                "{db} dB produced {}, expected {expected}",
                out[127]
            );
        }
    }

    #[test]
    fn bypass_passes_the_signal_through_untouched() {
        let mut gain = Gain::default();
        gain.params.gain.set_plain(-24.0);
        gain.params.bypass.set(true);
        gain.prepare(&config()).expect("a valid config");
        let out = run(&mut gain, 64, 0.5);
        assert!(
            (out[63] - 0.5).abs() < 1e-6,
            "bypassed output is {}",
            out[63]
        );
    }

    #[test]
    fn the_meter_is_written_by_the_audio_thread_and_read_without_a_lock() {
        let mut gain = Gain::default();
        gain.params.gain.set_plain(0.0);
        gain.prepare(&config()).expect("a valid config");
        assert_eq!(gain.params.output.value(), -60.0, "cleared by prepare");

        run(&mut gain, 128, 0.5);
        let level = gain.params.output.value();
        // 0.5 is -6.02 dBFS.
        assert!(
            (level - -6.02).abs() < 0.1,
            "the meter reads {level}, expected about -6 dBFS"
        );

        // The peak is held rather than replaced, so a quiet block after a loud one does not
        // hide the transient from a UI that repaints at 60 Hz.
        run(&mut gain, 128, 0.01);
        assert!(
            gain.params.output.value() > -20.0,
            "the held peak was overwritten by a quieter block"
        );
    }

    #[test]
    fn process_never_allocates() {
        assert!(
            counting_allocator_installed(),
            "the tripwire is not installed, so this test would pass vacuously"
        );
        let mut gain = Gain::default();
        gain.prepare(&config()).expect("a valid config");

        let frames = 256;
        let mut input = AudioStorage::<f32>::new(2, frames);
        input.fill(0.25);
        let mut output = AudioStorage::<f32>::new(2, frames);
        let mut events = EventBuffer::with_capacity(4, 64);
        events
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(64),
                param_id: param_id::GAIN,
                value: -6.0,
                ..ParamEvent::default()
            }))
            .expect("room");

        let cfg = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(frames, &cfg, &host);
        let inputs = [input.as_ref()];
        let mut outputs = [output.as_mut()];
        let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
        let mut sink = EventBuffer::with_capacity(4, 64);
        let mut ports = ProcessEvents::new(&events, &mut sink);

        let (_, allocations) = AllocGuard::scope(|| gain.process(&ctx, &mut buses, &mut ports));
        assert_eq!(allocations, 0, "process allocated {allocations} time(s)");
    }

    // ---- the editor ------------------------------------------------------------------------

    #[test]
    fn the_editor_opens_paints_resizes_and_closes() {
        let mut gain = Gain::default();
        let boxed = gain.create_editor().expect("this plug-in has an editor");
        let mut editor = downcast_editor(boxed).expect("wrapped with `editor(..)`");

        assert_eq!(editor.descriptor().preferred_size, EDITOR_SIZE);
        let host = HostServices::null();
        open(editor.as_mut(), &host);
        editor.tick();
        editor.tick();
        editor
            .resize(PhysicalSize::new(400, 240))
            .expect("a resize to a real size must succeed");
        assert!(
            editor.resize(PhysicalSize::new(0, 0)).is_err(),
            "an empty size must be refused rather than producing a zero surface"
        );
        editor.close();
    }

    #[test]
    fn an_editor_can_be_opened_and_closed_repeatedly_while_the_dsp_runs() {
        // `CLAUDE.md` rule 9: the editor's lifetime is independent of the processor's.
        let mut gain = Gain::default();
        gain.params.gain.set_plain(-6.0);
        gain.prepare(&config()).expect("a valid config");
        let host = HostServices::null();

        for _ in 0..3 {
            let boxed = gain.create_editor().expect("an editor every time");
            let mut editor = downcast_editor(boxed).expect("wrapped with `editor(..)`");
            open(editor.as_mut(), &host);
            editor.tick();
            editor.close();
            drop(editor);

            // Audio keeps working, at the same gain, after the editor is gone.
            let out = run(&mut gain, 64, 1.0);
            let expected = db_to_gain(-6.0);
            assert!(
                (out[63] - expected).abs() < 1e-4,
                "closing the editor disturbed the DSP: {}",
                out[63]
            );
        }
    }

    #[test]
    fn the_editor_and_the_processor_share_one_parameter_bank() {
        let mut gain = Gain::default();
        gain.prepare(&config()).expect("a valid config");
        let bank = Arc::clone(gain.params());

        // A "UI" write, from another handle to the same bank.
        bank.gain.set_plain(-12.0);
        // The parameter is smoothed over 15 ms, which is 720 samples at 48 kHz — the ramp is
        // the point of the smoother, so the test waits it out rather than pretending it is
        // not there.
        let mut out = Vec::new();
        for _ in 0..4 {
            out = run(&mut gain, 256, 1.0);
        }
        let expected = db_to_gain(-12.0);
        assert!(
            (out[255] - expected).abs() < 1e-4,
            "the processor did not see the UI's value: {}",
            out[255]
        );

        // …and the audio thread's meter is visible from the UI handle.
        assert!(bank.output.value() > -60.0);
    }

    #[test]
    fn a_drag_produces_one_balanced_gesture_and_never_latches_the_lane() {
        // The failure this guards against is silent: a `begin` with no matching `end` leaves
        // the host recording automation forever.
        let (mut gain, spy) = with_spy();
        let boxed = gain.create_editor().expect("an editor");
        let mut editor = downcast_editor(boxed).expect("wrapped with `editor(..)`");
        let host = HostServices::null();
        open(editor.as_mut(), &host);
        editor.tick();

        // Press on the knob, drag upwards, release. The knob is the first widget under the
        // heading, so a point well inside the top-left quadrant of the body hits it.
        let at = LogicalPoint::new(40.0, 70.0);
        editor.on_input(&InputEvent::PointerMoved {
            position: at,
            modifiers: Modifiers::NONE,
        });
        editor.on_input(&InputEvent::PointerButton {
            position: at,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        editor.tick();
        editor.on_input(&InputEvent::PointerMoved {
            position: LogicalPoint::new(40.0, 20.0),
            modifiers: Modifiers::NONE,
        });
        editor.tick();
        editor.on_input(&InputEvent::PointerButton {
            position: LogicalPoint::new(40.0, 20.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        editor.tick();
        editor.close();
        drop(editor);

        let calls = spy.calls();
        let begins = calls.iter().filter(|c| c.starts_with("begin")).count();
        let ends = calls.iter().filter(|c| c.starts_with("end")).count();
        assert_eq!(begins, ends, "every gesture must be closed, got {calls:?}");
    }

    #[test]
    fn the_binding_reports_the_value_the_parameter_actually_took() {
        // The contract every widget relies on: the host hears the clamped value, not the one
        // the pointer asked for. A host told the unclamped value draws its automation lane out
        // of step with the audio.
        let (gain, spy) = with_spy();
        let host = gain.host.clone();
        {
            let binding = ParamBinding::new(&gain.params.gain, host.params());
            binding.begin_gesture();
            binding.set_plain(1_000.0); // far above the +12 dB ceiling
            binding.end_gesture();
        }
        assert_eq!(gain.params.gain.value(), 12.0);
        assert_eq!(spy.calls(), ["begin 1", "changed 1 12", "end 1"]);
    }

    #[test]
    fn an_editor_torn_down_mid_drag_closes_its_gesture() {
        let (gain, spy) = with_spy();
        let host = gain.host.clone();
        {
            let binding = ParamBinding::new(&gain.params.gain, host.params());
            binding.begin_gesture();
            binding.set_normalized(0.75);
            // The user is still holding the control when the editor is destroyed.
        }
        assert_eq!(
            spy.calls().last().map(String::as_str),
            Some("end 1"),
            "a gesture left open records automation forever"
        );
    }

    #[test]
    fn the_editor_works_with_a_host_that_offers_no_automation_service() {
        // `HostServices::params()` is an `Option` for a reason: a preview harness, an offline
        // render and several real hosts provide nothing at all.
        let mut gain = Gain::default();
        assert!(gain.host.params().is_none());
        let boxed = gain.create_editor().expect("an editor");
        let mut editor = downcast_editor(boxed).expect("wrapped with `editor(..)`");
        let host = HostServices::null();
        open(editor.as_mut(), &host);
        editor.tick();
        editor.close();
    }

    #[test]
    fn drawing_a_frame_by_hand_touches_every_control() {
        // `draw` is a free function precisely so this test does not need an editor at all.
        let params = GainParams::new();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| draw(ui, &params, None));
        assert!(
            !output.shapes.is_empty(),
            "the editor drew nothing at all, so no control was laid out"
        );
        // `TexturesDelta` asserts on drop that someone handled it — a painter would upload
        // the font atlas here. This test draws to nowhere, so it says so explicitly.
        output.textures_delta.clear();
    }

    // ---- metadata --------------------------------------------------------------------------

    #[test]
    fn the_descriptor_advertises_the_editor_it_really_has() {
        let d = <Gain as DauxPlugin>::descriptor();
        d.validate().expect("the descriptor must be valid");
        assert!(d.capabilities.is_audio_effect());
        assert!(
            d.capabilities.is_has_gui(),
            "the plug-in creates an editor, so it must say so"
        );
        assert!(
            !d.capabilities.is_requires_gui(),
            "the DSP runs perfectly well headless"
        );
        assert!(Gain::default().create_editor().is_some());
    }

    #[test]
    fn state_round_trips_and_leaves_the_meter_out_of_it() {
        let gain = Gain::default();
        gain.params.gain.set_plain(-9.0);
        gain.params.bypass.set(true);
        gain.params.output.set_value(-3.0);

        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        gain.save_state(&mut writer).expect("saving cannot fail");
        let blob = writer.finish();

        let mut restored = Gain::default();
        restored.params.output.set_value(-42.0);
        let reader = StateReader::from_bytes(&blob).expect("the blob we wrote parses");
        restored.load_state(&reader).expect("loading cannot fail");

        assert!((restored.params.gain.value() - -9.0).abs() < 1e-9);
        assert!(restored.params.bypass.value());
        assert_eq!(
            restored.params.output.value(),
            -42.0,
            "a meter describes the audio, not the user's intent, and must not be restored"
        );
    }

    #[test]
    fn every_parameter_is_reachable_by_its_permanent_id() {
        let params = GainParams::new();
        assert_eq!(params.param_refs().len(), 3);
        for id in [param_id::GAIN, param_id::BYPASS, param_id::OUTPUT] {
            assert!(
                params.param(ParamId::new(id)).is_some(),
                "id {id} is missing"
            );
        }
        assert!(params.param(ParamId::new(99)).is_none());
        assert!(
            params
                .output
                .info()
                .flags
                .contains(daux_plugin::ParamFlags::IS_METER),
            "a meter must be flagged so a host does not offer it as an automation target"
        );
    }
}
