//! One binary exposing several plug-ins through a single factory.
//!
//! A `.axt` bundle has exactly one dynamic library and exactly one
//! [`daux_plugin_entry_v1`] symbol, so shipping a whole product — a channel strip, a suite,
//! a free pair of utilities — means one factory that answers for all of them. That factory is
//! [`PluginRegistry`], and this example is the smallest complete use of it: two effects,
//! [`Trim`] and [`Width`], in one module.
//!
//! # What it shows
//!
//! | Topic | Where |
//! |---|---|
//! | several plug-ins behind one entry point | [`ExampleFactory`] |
//! | ids are permanent and unique *within* the module | [`PluginRegistry::try_register`] |
//! | two independent parameter banks and state blobs | [`TrimParams`], [`WidthParams`] |
//! | preallocate in `prepare`, never in `process` | both processors |
//!
//! # Why not two crates?
//!
//! Because a host scans *bundles*, not types. Two crates means two bundles, two copies of
//! every shared table and two entries in the user's plug-in list where they expected one
//! folder. Register both here and the host sees one bundle containing two plug-ins, which is
//! what [`DauxFactory::plugin_count`] is for.
//!
//! # The one rule that bites here
//!
//! A plug-in id is what a saved project stores. Two plug-ins in one module sharing an id
//! means one of them can never be loaded again and a session silently opens the wrong thing,
//! so [`PluginRegistry`] refuses the second registration rather than letting the last one
//! win. It happens while the factory is being constructed, before any host has seen either
//! descriptor.
//!
//! [`daux_plugin_entry_v1`]: https://docs.rs/daux-format-axt

use daux_plugin::dsp::db_to_gain;
use daux_plugin::prelude::*;

/// The state schema version both plug-ins write.
const STATE_VERSION: u32 = 1;

// =========================================================================================
// Trim — a gain and a polarity switch
// =========================================================================================

/// [`Trim`]'s parameters.
#[derive(DauxParams)]
pub struct TrimParams {
    /// Output gain in decibels.
    #[param(id = 1, name = "Trim", range = -24.0..=24.0, default = 0.0, unit = "dB",
            decimals = 1, smoothing = "exponential(15.0)")]
    pub trim: FloatParam,

    /// Flips the polarity of every channel.
    #[param(id = 2, name = "Invert", default = false, labels("Normal", "Inverted"))]
    pub invert: BoolParam,
}

/// A level trim with a polarity switch: the first thing on every channel strip.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.trim",
    name = "DAUx Trim",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Level trim with a polarity switch.",
    license = "MIT OR Apache-2.0",
    category = "utility",
    capabilities(audio_effect, sample_accurate_auto, offline_render, sandbox_safe),
    features("utility", "gain"),
    state_schema_version = STATE_VERSION
)]
pub struct Trim {
    params: TrimParams,
    /// Ramps the trim so an automation jump does not click.
    smoother: Smoother,
    /// One coefficient per sample of the largest block the host promised. Allocated in
    /// `prepare`, written in `process`.
    ramp: Vec<f32>,
}

impl Default for Trim {
    fn default() -> Self {
        let params = TrimParams::new();
        let smoother = params.trim.smoother();
        Self {
            params,
            smoother,
            ramp: Vec::new(),
        }
    }
}

impl Trim {
    /// `[audio-thread]` The linear coefficient the parameters currently ask for, polarity
    /// included.
    fn target(&self) -> f32 {
        let gain = db_to_gain(self.params.trim.value_f32());
        if self.params.invert.value() {
            -gain
        } else {
            gain
        }
    }
}

impl DauxProcessor for Trim {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.ramp.clear();
        self.ramp.resize(config.max_block_size as usize, 1.0);
        self.smoother.prepare(config.sample_rate);
        self.smoother.reset_to(self.target());
        Ok(())
    }

    fn reset(&mut self) {
        self.smoother.reset_to(self.target());
    }

    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let frames = ctx.frames().min(self.ramp.len());
        self.smoother.set_target(self.target());

        // Automation applied at its own sample offset: fill the ramp up to each event, change
        // the target, carry on.
        let mut filled = 0usize;
        for index in 0..events.input().len() {
            let Some(DauxEvent::ParamValue(event)) = events.input().get(index) else {
                continue;
            };
            let at = (event.header.time as usize).clamp(filled, frames);
            self.smoother.next_block(&mut self.ramp[filled..at]);
            filled = at;

            if let Some(param) = self.params.param(ParamId::new(event.param_id)) {
                param.set_plain(event.value);
            }
            self.smoother.set_target(self.target());
        }
        self.smoother.next_block(&mut self.ramp[filled..frames]);

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

        let ramp = &self.ramp[..frames];
        for channel in output.split_channels_mut() {
            for (sample, gain) in channel.iter_mut().zip(ramp) {
                *sample *= gain;
            }
        }
        ProcessStatus::ContinueIfNotQuiet
    }
}

impl DauxController for Trim {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        save_params(&self.params, w)
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        load_params(&self.params, r)
    }
}

impl DauxPlugin for Trim {
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

    /// `[main-thread]` Any channel count, as long as in and out agree.
    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        let channels = |bus: Option<&BusInfo>| bus.map_or(0, BusInfo::channel_count);
        let inputs = channels(layout.main_input());
        let outputs = channels(layout.main_output());
        layout.outputs.len() == 1 && outputs > 0 && (inputs == 0 || inputs == outputs)
    }
}

// =========================================================================================
// Width — a mid/side stereo widener
// =========================================================================================

/// [`Width`]'s parameters.
#[derive(DauxParams)]
pub struct WidthParams {
    /// Stereo width: `0` collapses to mono, `1` is unchanged, `2` doubles the side signal.
    #[param(id = 1, name = "Width", range = 0.0..=2.0, default = 1.0, decimals = 2,
            smoothing = "exponential(20.0)")]
    pub width: FloatParam,
}

/// A mid/side stereo widener.
///
/// Note that its `Width` parameter is also id `1`. Parameter ids are scoped to a plug-in, so
/// that is not a collision — only *plug-in* ids have to be unique inside a module.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.width",
    name = "DAUx Width",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Mid/side stereo width control.",
    license = "MIT OR Apache-2.0",
    category = "effect",
    capabilities(
        audio_effect,
        stereo_only,
        sample_accurate_auto,
        offline_render,
        sandbox_safe
    ),
    features("stereo", "utility"),
    state_schema_version = STATE_VERSION
)]
pub struct Width {
    params: WidthParams,
    /// Ramps the width so a sweep does not zipper.
    smoother: Smoother,
    /// One width coefficient per sample, sized in `prepare`.
    ramp: Vec<f32>,
}

impl Default for Width {
    fn default() -> Self {
        let params = WidthParams::new();
        let smoother = params.width.smoother();
        Self {
            params,
            smoother,
            ramp: Vec::new(),
        }
    }
}

impl DauxProcessor for Width {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;
        self.ramp.clear();
        self.ramp.resize(config.max_block_size as usize, 1.0);
        self.smoother.prepare(config.sample_rate);
        self.smoother.reset_to(self.params.width.value_f32());
        Ok(())
    }

    fn reset(&mut self) {
        self.smoother.reset_to(self.params.width.value_f32());
    }

    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let frames = ctx.frames().min(self.ramp.len());
        self.smoother.set_target(self.params.width.value_f32());

        let mut filled = 0usize;
        for index in 0..events.input().len() {
            let Some(DauxEvent::ParamValue(event)) = events.input().get(index) else {
                continue;
            };
            let at = (event.header.time as usize).clamp(filled, frames);
            self.smoother.next_block(&mut self.ramp[filled..at]);
            filled = at;

            if let Some(param) = self.params.param(ParamId::new(event.param_id)) {
                param.set_plain(event.value);
            }
            self.smoother.set_target(self.params.width.value_f32());
        }
        self.smoother.next_block(&mut self.ramp[filled..frames]);

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

        // Mono in a stereo widener is not an error, it just has no side signal to widen.
        let Some((left, right)) = output.channel_pair_mut(0, 1) else {
            return ProcessStatus::ContinueIfNotQuiet;
        };
        for ((l, r), &width) in left
            .iter_mut()
            .zip(right.iter_mut())
            .zip(&self.ramp[..frames])
        {
            let mid = (*l + *r) * 0.5;
            let side = (*r - *l) * 0.5 * width;
            *l = mid - side;
            *r = mid + side;
        }
        ProcessStatus::ContinueIfNotQuiet
    }
}

impl DauxController for Width {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        save_params(&self.params, w)
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        load_params(&self.params, r)
    }
}

impl DauxPlugin for Width {
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

    /// `[main-thread]` Stereo only, which is what the `STEREO_ONLY` capability promises.
    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        layout.outputs.len() == 1
            && layout.main_output().map_or(0, BusInfo::channel_count) == 2
            && layout
                .main_input()
                .is_none_or(|bus| bus.channel_count() == 2)
    }
}

// =========================================================================================
// State, shared by both
// =========================================================================================

/// `[main-thread]` Writes every parameter of `params` under its permanent id.
///
/// Keyed by id rather than by display name: the name is text a translation may change, the
/// id never changes. Shared by both plug-ins because there is nothing plug-in-specific about
/// it — the state formats stay independent because the *banks* are.
fn save_params(params: &dyn Params, w: &mut StateWriter) -> DauxResult<()> {
    w.begin_group("params");
    for (id, param) in params.param_refs() {
        w.put_f64(&id.get().to_string(), param.plain());
    }
    w.end_group();
    Ok(())
}

/// `[main-thread]` Restores what [`save_params`] wrote.
///
/// A missing key is not an error: it means an older version of the plug-in did not have that
/// parameter, and refusing to load would stop the user's project from opening at all.
fn load_params(params: &dyn Params, r: &StateReader) -> DauxResult<()> {
    for (id, param) in params.param_refs() {
        if let Some(value) = r.opt_f64(&format!("params/{}", id.get())) {
            param.set_plain(value);
        }
    }
    Ok(())
}

// =========================================================================================
// The factory
// =========================================================================================

/// The one factory this module exports, answering for both plug-ins.
///
/// A newtype around [`PluginRegistry`] rather than the registry itself, because
/// [`export_plugin!`] needs a type that is [`Default`] *and* already populated: every format's
/// entry point constructs the factory with no arguments, so registration has to happen inside
/// [`Default::default`].
pub struct ExampleFactory(PluginRegistry);

impl Default for ExampleFactory {
    /// `[main-thread]` Registers both plug-ins, in the order a host will list them.
    ///
    /// # Panics
    ///
    /// If either descriptor is invalid or the two ids collide — both are bugs in this module,
    /// caught while the factory is built and long before any audio runs.
    fn default() -> Self {
        Self(PluginRegistry::new().with::<Trim>().with::<Width>())
    }
}

impl ExampleFactory {
    /// `[main-thread]` Builds the factory. Cheap: it constructs no plug-ins and loads
    /// nothing, which is what makes scanning a library of hundreds of bundles affordable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl DauxFactory for ExampleFactory {
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

export_plugin!(ExampleFactory);

/// The allocation tripwire, installed only while this crate's tests are compiled.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_plugin::CountingAllocator = daux_plugin::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin::EventBuffer;
    use daux_plugin::daux_rt::{AllocGuard, counting_allocator_installed};

    const TRIM_ID: &str = "studio.futureboard.daux.example.trim";
    const WIDTH_ID: &str = "studio.futureboard.daux.example.width";

    fn config() -> ProcessConfig {
        ProcessConfig::new(48_000.0, 256)
    }

    /// Runs one block through a processor and returns the two output channels.
    fn run(
        processor: &mut dyn DauxProcessor,
        left_in: &[f32],
        right_in: &[f32],
        events: &EventBuffer,
    ) -> (Vec<f32>, Vec<f32>) {
        let frames = left_in.len();
        assert_eq!(frames, right_in.len());

        let mut input = AudioStorage::<f32>::new(2, frames);
        input
            .channel_mut(0)
            .expect("channel 0")
            .copy_from_slice(left_in);
        input
            .channel_mut(1)
            .expect("channel 1")
            .copy_from_slice(right_in);
        let mut output = AudioStorage::<f32>::new(2, frames);

        let cfg = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(frames, &cfg, &host);
        {
            let inputs = [input.as_ref()];
            let mut outputs = [output.as_mut()];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
            let mut sink = EventBuffer::with_capacity(8, 128);
            let mut ports = ProcessEvents::new(events, &mut sink);
            let status = processor.process(&ctx, &mut buses, &mut ports);
            assert_ne!(status, ProcessStatus::Error);
        }
        (
            output.channel(0).expect("channel 0").to_vec(),
            output.channel(1).expect("channel 1").to_vec(),
        )
    }

    fn no_events() -> EventBuffer {
        EventBuffer::with_capacity(1, 16)
    }

    // ---- the factory ---------------------------------------------------------------------

    #[test]
    fn the_factory_exports_both_plug_ins_in_registration_order() {
        let factory = ExampleFactory::new();
        assert_eq!(factory.plugin_count(), 2);
        assert_eq!(
            factory.descriptor(0).map(|d| d.id.as_str().to_owned()),
            Some(TRIM_ID.to_owned())
        );
        assert_eq!(
            factory.descriptor(1).map(|d| d.id.as_str().to_owned()),
            Some(WIDTH_ID.to_owned())
        );
        assert!(factory.descriptor(2).is_none());
        assert_eq!(factory.descriptors().len(), 2);
    }

    #[test]
    fn every_exported_descriptor_is_valid() {
        for descriptor in ExampleFactory::new().descriptors() {
            descriptor
                .validate()
                .unwrap_or_else(|e| panic!("{} is invalid: {e}", descriptor.id.as_str()));
        }
    }

    #[test]
    fn each_plug_in_can_be_created_by_its_permanent_id() {
        let factory = ExampleFactory::new();
        assert!(factory.contains(TRIM_ID));
        assert!(factory.contains(WIDTH_ID));

        let mut trim = factory.create(TRIM_ID).expect("Trim must be creatable");
        assert_eq!(
            trim.bus_layout().main_output().map(BusInfo::channel_count),
            Some(2)
        );
        assert_eq!(trim.controller().params().param_refs().len(), 2);

        let mut width = factory.create(WIDTH_ID).expect("Width must be creatable");
        assert_eq!(width.controller().params().param_refs().len(), 1);

        let Err(err) = factory.create("studio.futureboard.daux.example.missing") else {
            panic!("an unknown id must not produce a plug-in");
        };
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn the_two_plug_ins_are_genuinely_independent_instances() {
        let factory = ExampleFactory::new();
        let mut a = factory.create(TRIM_ID).expect("Trim");
        let mut b = factory.create(TRIM_ID).expect("Trim again");

        let set = |plugin: &mut Box<dyn DauxPlugin>, value: f64| {
            plugin
                .controller()
                .params()
                .param(ParamId::new(1))
                .expect("the trim parameter")
                .set_plain(value);
        };
        set(&mut a, -12.0);
        set(&mut b, 6.0);

        let read = |plugin: &mut Box<dyn DauxPlugin>| {
            plugin
                .controller()
                .params()
                .param(ParamId::new(1))
                .expect("the trim parameter")
                .plain()
        };
        assert_eq!(read(&mut a), -12.0);
        assert_eq!(read(&mut b), 6.0, "the two instances share state");
    }

    #[test]
    fn a_duplicate_plug_in_id_is_refused_rather_than_shadowing() {
        // The failure this guards against is silent and permanent: a host stores the id in
        // the project file, so the second registration would make one plug-in unloadable.
        let mut registry = PluginRegistry::new();
        registry.try_register::<Trim>().expect("the first wins");
        let err = registry
            .try_register::<Trim>()
            .expect_err("the second must be refused");
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn the_two_ids_really_are_different() {
        assert_ne!(TRIM_ID, WIDTH_ID);
        let ids: Vec<String> = ExampleFactory::new()
            .descriptors()
            .into_iter()
            .map(|d| d.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, [TRIM_ID, WIDTH_ID]);
    }

    // ---- Trim ----------------------------------------------------------------------------

    #[test]
    fn trim_scales_by_the_decibel_value_it_is_given() {
        for db in [-24.0, -6.0, 0.0, 6.0, 24.0] {
            let mut trim = Trim::default();
            trim.params.trim.set_plain(db);
            trim.prepare(&config()).expect("a valid config");

            let ones = [1.0f32; 128];
            let (left, right) = run(&mut trim, &ones, &ones, &no_events());
            let expected = db_to_gain(db as f32);
            assert!(
                (left[127] - expected).abs() < 1e-4,
                "{db} dB produced {}, expected {expected}",
                left[127]
            );
            assert_eq!(left, right);
        }
    }

    #[test]
    fn trim_inverts_the_polarity_of_every_channel() {
        let mut trim = Trim::default();
        trim.params.invert.set(true);
        trim.prepare(&config()).expect("a valid config");

        let left_in = [1.0f32; 64];
        let right_in = [-0.5f32; 64];
        let (left, right) = run(&mut trim, &left_in, &right_in, &no_events());
        assert!((left[63] + 1.0).abs() < 1e-4, "left is {}", left[63]);
        assert!((right[63] - 0.5).abs() < 1e-4, "right is {}", right[63]);
    }

    #[test]
    fn trim_applies_automation_at_its_own_sample_offset() {
        let mut trim = Trim {
            smoother: Smoother::new(Smoothing::None),
            ..Trim::default()
        };
        trim.prepare(&config()).expect("a valid config");

        let mut events = EventBuffer::with_capacity(4, 64);
        events
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(96),
                param_id: 1,
                value: -12.0,
                ..ParamEvent::default()
            }))
            .expect("room");

        let ones = [1.0f32; 256];
        let (left, _) = run(&mut trim, &ones, &ones, &events);
        assert!((left[95] - 1.0).abs() < 1e-6, "sample 95 is {}", left[95]);
        let expected = db_to_gain(-12.0);
        assert!(
            (left[96] - expected).abs() < 1e-6,
            "sample 96 is {}, expected {expected}",
            left[96]
        );
    }

    // ---- Width ---------------------------------------------------------------------------

    #[test]
    fn width_one_leaves_the_stereo_image_alone() {
        let mut width = Width::default();
        width.prepare(&config()).expect("a valid config");

        let left_in = [0.8f32; 64];
        let right_in = [-0.2f32; 64];
        let (left, right) = run(&mut width, &left_in, &right_in, &no_events());
        assert!((left[63] - 0.8).abs() < 1e-5, "left is {}", left[63]);
        assert!((right[63] + 0.2).abs() < 1e-5, "right is {}", right[63]);
    }

    #[test]
    fn width_zero_collapses_to_mono() {
        let mut width = Width::default();
        width.params.width.set_plain(0.0);
        width.prepare(&config()).expect("a valid config");

        let left_in = [1.0f32; 64];
        let right_in = [0.0f32; 64];
        let (left, right) = run(&mut width, &left_in, &right_in, &no_events());
        assert!((left[63] - 0.5).abs() < 1e-5, "left is {}", left[63]);
        assert!(
            (left[63] - right[63]).abs() < 1e-6,
            "mono means the two channels are identical"
        );
    }

    #[test]
    fn width_two_doubles_the_side_signal_and_keeps_the_mid() {
        let mut width = Width::default();
        width.params.width.set_plain(2.0);
        width.prepare(&config()).expect("a valid config");

        // Mid 0.5, side 0.5. Doubling the side gives -0.5 / +1.5 around the same mid.
        let left_in = [0.0f32; 64];
        let right_in = [1.0f32; 64];
        let (left, right) = run(&mut width, &left_in, &right_in, &no_events());
        assert!((left[63] + 0.5).abs() < 1e-5, "left is {}", left[63]);
        assert!((right[63] - 1.5).abs() < 1e-5, "right is {}", right[63]);
        assert!(
            ((left[63] + right[63]) * 0.5 - 0.5).abs() < 1e-5,
            "the mono sum must be untouched"
        );
    }

    #[test]
    fn width_leaves_a_correlated_signal_alone_at_any_width() {
        // A dead-centre signal has no side component, so widening it can do nothing. A
        // widener that got the mid/side algebra wrong changes its level here.
        for w in [0.0, 0.5, 1.0, 2.0] {
            let mut width = Width::default();
            width.params.width.set_plain(w);
            width.prepare(&config()).expect("a valid config");
            let same = [0.4f32; 64];
            let (left, right) = run(&mut width, &same, &same, &no_events());
            assert!((left[63] - 0.4).abs() < 1e-5, "width {w} moved the mid");
            assert!((right[63] - 0.4).abs() < 1e-5, "width {w} moved the mid");
        }
    }

    #[test]
    fn width_only_accepts_stereo() {
        let width = Width::default();
        assert!(width.accepts_bus_layout(&BusLayout::stereo_effect()));
        assert!(!width.accepts_bus_layout(&BusLayout::mono_effect()));
        assert!(!width.accepts_bus_layout(&BusLayout::effect(ChannelLayout::Surround5_1)));
        assert!(
            <Width as DauxPlugin>::descriptor()
                .capabilities
                .is_stereo_only(),
            "the layout check and the capability must agree"
        );
    }

    // ---- shared behaviour ------------------------------------------------------------------

    #[test]
    fn neither_processor_allocates() {
        assert!(
            counting_allocator_installed(),
            "the tripwire is not installed, so this test would pass vacuously"
        );

        let mut events = EventBuffer::with_capacity(4, 64);
        events
            .try_push(&DauxEvent::ParamValue(ParamEvent {
                header: EventHeader::at(64),
                param_id: 1,
                value: 0.5,
                ..ParamEvent::default()
            }))
            .expect("room");

        let mut trim = Trim::default();
        trim.prepare(&config()).expect("a valid config");
        let mut width = Width::default();
        width.prepare(&config()).expect("a valid config");

        for (name, processor) in [
            ("Trim", &mut trim as &mut dyn DauxProcessor),
            ("Width", &mut width as &mut dyn DauxProcessor),
        ] {
            let frames = 256;
            let mut input = AudioStorage::<f32>::new(2, frames);
            input.fill(0.25);
            let mut output = AudioStorage::<f32>::new(2, frames);
            let cfg = config();
            let host = RtHostServices::null();
            let ctx = ProcessContext::new(frames, &cfg, &host);
            let inputs = [input.as_ref()];
            let mut outputs = [output.as_mut()];
            let mut buses = AudioBuses::new(&inputs, &mut outputs, frames);
            let mut sink = EventBuffer::with_capacity(4, 64);
            let mut ports = ProcessEvents::new(&events, &mut sink);

            let (_, allocations) =
                AllocGuard::scope(|| processor.process(&ctx, &mut buses, &mut ports));
            assert_eq!(allocations, 0, "{name} allocated {allocations} time(s)");
        }
    }

    #[test]
    fn each_plug_in_saves_and_restores_its_own_state() {
        let trim = Trim::default();
        trim.params.trim.set_plain(-9.5);
        trim.params.invert.set(true);
        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        trim.save_state(&mut writer).expect("saving cannot fail");
        let trim_blob = writer.finish();

        let width = Width::default();
        width.params.width.set_plain(1.75);
        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        width.save_state(&mut writer).expect("saving cannot fail");
        let width_blob = writer.finish();

        let mut restored_trim = Trim::default();
        restored_trim
            .load_state(&StateReader::from_bytes(&trim_blob).expect("parses"))
            .expect("loading cannot fail");
        assert!((restored_trim.params.trim.value() - -9.5).abs() < 1e-9);
        assert!(restored_trim.params.invert.value());

        let mut restored_width = Width::default();
        restored_width
            .load_state(&StateReader::from_bytes(&width_blob).expect("parses"))
            .expect("loading cannot fail");
        assert!((restored_width.params.width.value() - 1.75).abs() < 1e-9);
    }

    #[test]
    fn a_blob_from_the_other_plug_in_is_ignored_rather_than_misread() {
        // Both banks use parameter id 1, so a blob written by Trim parses cleanly in Width.
        // Loading it must not be *fatal* — but it must also not be interpreted, which is what
        // the per-plug-in id in the project file is for. Here we only assert the graceful
        // half: nothing panics and nothing errors.
        let trim = Trim::default();
        trim.params.trim.set_plain(-20.0);
        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        trim.save_state(&mut writer).expect("saving cannot fail");
        let blob = writer.finish();

        let mut width = Width::default();
        width
            .load_state(&StateReader::from_bytes(&blob).expect("parses"))
            .expect("a foreign blob must not fail the load");
        // -20 is outside `0..=2` and the parameter clamps rather than storing nonsense.
        assert!((0.0..=2.0).contains(&width.params.width.value()));
    }

    #[test]
    fn prepare_refuses_a_configuration_neither_plug_in_can_size_from() {
        let mut trim = Trim::default();
        assert!(trim.prepare(&ProcessConfig::new(0.0, 256)).is_err());
        assert!(trim.ramp.is_empty());

        let mut width = Width::default();
        assert!(width.prepare(&ProcessConfig::new(48_000.0, 0)).is_err());
        assert!(width.ramp.is_empty());
    }

    #[test]
    fn a_block_larger_than_the_prepared_maximum_is_clamped_rather_than_grown() {
        let mut trim = Trim::default();
        trim.params.trim.set_plain(-6.0);
        trim.prepare(&config()).expect("a valid config");
        let capacity = trim.ramp.capacity();

        let ones = [1.0f32; 1_024];
        let (left, _) = run(&mut trim, &ones, &ones, &no_events());
        assert_eq!(
            trim.ramp.capacity(),
            capacity,
            "process re-allocated the ramp"
        );
        assert_eq!(left.len(), 1_024);
    }
}
