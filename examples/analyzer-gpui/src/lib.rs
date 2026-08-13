//! Spectrum analyzer with a GPUI editor (experimental).
//!
//! A constant-Q filter bank measures the input, and a GPUI editor draws it. The audio never
//! changes: the plug-in is a pure observer that passes its input through untouched.
//!
//! # What it shows
//!
//! | Topic | Where |
//! |---|---|
//! | **audio → UI with no lock**: an array of atomics behind an `Arc` | [`Spectrum`] |
//! | a filter bank sized once in `prepare`, never in `process` | [`Analyzer::prepare`] |
//! | a GPUI editor over the `futureboard/gpui-se` fork | [`SpectrumView`] |
//! | an editor whose lifetime is independent of the DSP's | [`Analyzer::create_editor`] |
//! | continuous repaint without a timer thread | [`SpectrumView::render`] |
//!
//! # The cross-thread rule
//!
//! The audio thread and the editor share exactly one object: an [`Arc<Spectrum>`], whose
//! bands are [`daux_rt::AtomicF32`]s. Publishing a band is one relaxed store; reading one is
//! one relaxed load. There is no lock, no channel, no allocation, and no way for the UI to
//! stall the audio thread — which is the entire reason the type looks like this.
//!
//! A `Mutex<Vec<f32>>` would be the obvious alternative and is a real-time bug even while
//! uncontended: the UI thread can be preempted while holding it, and the audio thread would
//! then wait on a descheduled thread with a deadline of a few milliseconds. `gpui_embedded`
//! ships the same idea under [`gpui_embedded::audio`] — either is fine, and a `Mutex` is not.
//!
//! The `Arc` is also what makes rule 9 work: the editor may be opened and closed dozens of
//! times over the plug-in's life, and each new editor simply clones the handle. The audio
//! thread never learns that an editor exists at all.
//!
//! # Why the GPUI fork
//!
//! Upstream GPUI assumes it *is* the application: it creates windows, owns the event loop,
//! keeps global state and calls `exit`. All four are fatal in a plug-in. The
//! `futureboard/gpui-se` fork carries `gpui_embedded`, a GPUI platform that creates no
//! windows and owns no event loop — the host hands it a child view and tells it when to idle.
//! See `crates/daux-graphics-gpui`.
//!
//! # Experimental
//!
//! This example is excluded from `cargo build`'s default members because it drags in a GPU
//! stack. Build it explicitly:
//!
//! ```text
//! cargo build -p daux-example-analyzer-gpui
//! ```
//!
//! [`Arc<Spectrum>`]: std::sync::Arc

use std::sync::Arc;

use daux_plugin::dsp::{Biquad, PeakFollower, gain_to_db, simd};
use daux_plugin::graphics::gpui as backend;
use daux_plugin::prelude::*;

use backend::GpuiEditor;
use backend::gpui::{
    AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, px, rgb,
};
use daux_plugin::AtomicF32;

/// How many bands the filter bank measures.
///
/// A compile-time constant so [`Spectrum`] is a fixed-size array rather than a `Vec`: the
/// audio thread indexes it, and a `Vec` that could be resized from elsewhere would need a
/// lock to be safe.
pub const BAND_COUNT: usize = 24;

/// Lowest band centre, in hertz.
pub const LOWEST_HZ: f64 = 25.0;

/// Highest band centre, in hertz.
pub const HIGHEST_HZ: f64 = 16_000.0;

/// The level a band reads when it has heard nothing at all.
pub const SILENCE_DB: f32 = -96.0;

/// Bottom of the drawn range, in dBFS.
const FLOOR_DB: f32 = -72.0;

/// Top of the drawn range, in dBFS.
const CEILING_DB: f32 = 0.0;

/// The permanent parameter ids.
mod param_id {
    /// How fast a band falls, in milliseconds.
    pub const RELEASE: u32 = 1;
    /// Whether the analyzer passes audio through or mutes it.
    pub const MUTE: u32 = 2;
}

/// The state schema version [`Analyzer::save_state`] writes.
const STATE_VERSION: u32 = 1;

/// The editor's preferred size, in logical pixels.
const EDITOR_SIZE: LogicalSize = LogicalSize {
    width: 520.0,
    height: 300.0,
};

/// Height of the drawn bars' plot area, in logical pixels.
const PLOT_HEIGHT: f32 = 200.0;

/// The measurement the audio thread publishes and the editor reads.
///
/// One [`daux_rt::AtomicF32`] per band. Writing is a relaxed store and reading is a relaxed
/// load, so neither side can block the other and neither side allocates. There is deliberately
/// no `&mut self` method on this type: both halves only ever hold a `&Spectrum` through an
/// [`Arc`](std::sync::Arc).
///
/// `[any-thread]`
#[derive(Debug)]
pub struct Spectrum {
    bands: [AtomicF32; BAND_COUNT],
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    /// `[main-thread]` A spectrum reading silence in every band.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bands: [const { AtomicF32::new(SILENCE_DB) }; BAND_COUNT],
        }
    }

    /// `[audio-thread]` Publishes one band's level, in dBFS.
    ///
    /// One relaxed store. Safe to call from `process`, and the editor will see it the next
    /// time it repaints — there is nothing to wake and nothing to wait for.
    pub fn publish(&self, band: usize, db: f32) {
        if let Some(slot) = self.bands.get(band) {
            slot.set(db);
        }
    }

    /// `[any-thread]` The level of one band, in dBFS, or [`SILENCE_DB`] for an unknown band.
    #[must_use]
    pub fn band(&self, band: usize) -> f32 {
        self.bands.get(band).map_or(SILENCE_DB, AtomicF32::get)
    }

    /// `[audio-thread]` Resets every band to silence.
    pub fn clear(&self) {
        for band in &self.bands {
            band.set(SILENCE_DB);
        }
    }
}

/// `[main-thread]` The centre frequency of band `index`, log-spaced across the audible range.
#[must_use]
pub fn band_center_hz(index: usize) -> f64 {
    let t = if BAND_COUNT <= 1 {
        0.0
    } else {
        index as f64 / (BAND_COUNT - 1) as f64
    };
    LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ).powf(t)
}

/// `[main-thread]` The Q that makes each band's skirts meet its neighbours'.
///
/// Constant-Q: every band is the same width in octaves, which is how a spectrum analyzer's
/// display is read. Derived from the band spacing rather than guessed, so changing
/// [`BAND_COUNT`] keeps the bank continuous.
#[must_use]
pub fn band_q() -> f64 {
    if BAND_COUNT <= 1 {
        return 1.0;
    }
    let ratio = (HIGHEST_HZ / LOWEST_HZ).powf(1.0 / (BAND_COUNT - 1) as f64);
    ratio.sqrt() / (ratio - 1.0 / ratio.sqrt())
}

/// The analyzer's parameters.
#[derive(DauxParams)]
pub struct AnalyzerParams {
    /// How long a band takes to fall back after a transient.
    #[param(id = param_id::RELEASE, name = "Release", range = 10.0..=2000.0, default = 300.0,
            unit = "ms", curve = "log", decimals = 0)]
    pub release: FloatParam,

    /// Silences the output while still measuring, for soloing another track.
    #[param(id = param_id::MUTE, name = "Mute", default = false, labels("Thru", "Muted"))]
    pub mute: BoolParam,
}

/// One band of the filter bank: a bandpass and the follower that watches it.
struct Band {
    filter: Biquad,
    follower: PeakFollower,
}

/// A constant-Q spectrum analyzer with a GPUI editor.
#[derive(DauxPlugin)]
#[plugin(
    id = "studio.futureboard.daux.example.analyzer",
    name = "DAUx Analyzer",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    description = "Constant-Q spectrum analyzer with a GPUI editor.",
    license = "MIT OR Apache-2.0",
    category = "analyzer",
    capabilities(analyzer, audio_effect, has_gui, offline_render, sandbox_safe),
    features("analyzer", "spectrum"),
    state_schema_version = STATE_VERSION
)]
pub struct Analyzer {
    /// The parameter bank.
    params: AnalyzerParams,
    /// What the editor reads. Cloned into every editor that is ever opened.
    spectrum: Arc<Spectrum>,
    /// The filter bank, built in `prepare` because it depends on the sample rate.
    bands: Vec<Band>,
    /// The mono sum of the input, sized in `prepare`.
    mono: Vec<f32>,
    /// Scratch for one band's filtered copy of [`Analyzer::mono`], sized in `prepare`.
    ///
    /// One buffer reused by every band rather than one per band: [`Biquad::process_block`]
    /// works in place, so the naive version would need `BAND_COUNT` buffers to avoid
    /// clobbering the source. Copying is cheaper than the memory, and neither allocates here.
    scratch: Vec<f32>,
    /// The rate `prepare` was called with, so the release time can be re-derived.
    sample_rate: f64,
    /// The release time the followers were last built with, so they are only rebuilt when the
    /// parameter really moved.
    release_ms: f64,
}

impl Default for Analyzer {
    /// `[main-thread]` A fresh, unprepared analyzer reading silence.
    fn default() -> Self {
        Self {
            params: AnalyzerParams::new(),
            spectrum: Arc::new(Spectrum::new()),
            bands: Vec::new(),
            mono: Vec::new(),
            scratch: Vec::new(),
            sample_rate: 48_000.0,
            release_ms: 0.0,
        }
    }
}

impl Analyzer {
    /// `[main-thread]` The measurement handle, for an editor or a test.
    #[must_use]
    pub fn spectrum(&self) -> &Arc<Spectrum> {
        &self.spectrum
    }

    /// `[audio-thread]` Re-derives the follower ballistics when the release time moved.
    ///
    /// Cheap and bounded — [`BAND_COUNT`] calls to a function that does two `exp`s — and it
    /// allocates nothing, which is what makes it legal here at all.
    fn retime_followers(&mut self) {
        let release = self.params.release.value();
        if (release - self.release_ms).abs() < f64::EPSILON {
            return;
        }
        self.release_ms = release;
        for band in &mut self.bands {
            // A fast attack and a slow release: a peak meter must catch a transient and then
            // let the eye read it.
            band.follower.set_times(self.sample_rate, 1.0, release);
        }
    }
}

impl DauxProcessor for Analyzer {
    /// `[main-thread]` Builds the filter bank and the scratch buffers. The only place this
    /// plug-in allocates.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {
        config.validate()?;

        let max_block = config.max_block_size as usize;
        self.mono.clear();
        self.mono.resize(max_block, 0.0);
        self.scratch.clear();
        self.scratch.resize(max_block, 0.0);

        self.sample_rate = config.sample_rate;
        let release = self.params.release.value();
        let q = band_q();
        // Nyquist moves with the sample rate, so a band above it is built at the highest
        // frequency the rate can represent rather than being left with NaN coefficients.
        let ceiling = config.sample_rate * 0.45;

        self.bands.clear();
        self.bands.reserve_exact(BAND_COUNT);
        for index in 0..BAND_COUNT {
            let centre = band_center_hz(index).min(ceiling);
            self.bands.push(Band {
                filter: Biquad::bandpass(config.sample_rate, centre, q),
                follower: PeakFollower::new(config.sample_rate, 1.0, release),
            });
        }
        self.release_ms = release;
        self.spectrum.clear();
        Ok(())
    }

    /// `[audio-thread]` Drops every filter's and follower's memory of past audio.
    fn reset(&mut self) {
        for band in &mut self.bands {
            band.filter.reset();
            band.follower.reset();
        }
        self.spectrum.clear();
    }

    /// `[audio-thread]` Measures the input and passes it through.
    fn process<'a>(
        &mut self,
        ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        _events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {
        let frames = ctx.frames().min(self.mono.len());
        self.retime_followers();

        // --- the mono sum the bank measures -----------------------------------------------
        let mono = &mut self.mono[..frames];
        mono.fill(0.0);
        if let Some(input) = audio.main_input() {
            let channels = input.channel_count().max(1);
            let scale = 1.0 / channels as f32;
            for channel in 0..input.channel_count() {
                let samples = input.channel(channel);
                for (sum, sample) in mono.iter_mut().zip(&samples[..frames.min(samples.len())]) {
                    *sum += sample * scale;
                }
            }
        }

        // --- the filter bank ---------------------------------------------------------------
        //
        // Every buffer here was sized in `prepare`; the loop is bounded by `BAND_COUNT` and
        // by `frames`, and nothing inside it allocates, locks or formats.
        for (index, band) in self.bands.iter_mut().enumerate() {
            let scratch = &mut self.scratch[..frames];
            simd::copy_from(scratch, mono);
            band.filter.process_block(scratch);
            let peak = band.follower.process_block(scratch);
            self.spectrum.publish(index, gain_to_db(peak));
        }

        // --- the audio itself ---------------------------------------------------------------
        let input = audio.main_input();
        let Some(mut output) = audio.main_output() else {
            // Measuring with nothing to pass through is perfectly valid for an analyzer.
            return ProcessStatus::Continue;
        };
        if self.params.mute.value() {
            output.fill_silence();
            return ProcessStatus::Continue;
        }
        if let Some(input) = input
            && output.copy_from(&input).is_err()
        {
            output.fill_silence();
            return ProcessStatus::Error;
        }

        // `Continue` rather than `ContinueIfNotQuiet`: the bands are still falling after the
        // input goes silent, and a host that stopped calling would freeze the display.
        ProcessStatus::Continue
    }

    /// `[audio-thread]` The analyzer adds no delay: the output is the input.
    fn latency(&self) -> Latency {
        Latency::Zero
    }
}

impl DauxController for Analyzer {
    fn params(&self) -> &dyn Params {
        &self.params
    }

    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()> {
        w.put_f64("release_ms", self.params.release.value());
        w.put_bool("mute", self.params.mute.value());
        // The spectrum is a measurement of the last few milliseconds of audio, not state.
        Ok(())
    }

    fn load_state(&mut self, r: &StateReader) -> DauxResult<()> {
        if let Some(ms) = r.opt_f64("release_ms") {
            self.params.release.set_plain(ms);
        }
        if let Some(mute) = r.opt_bool("mute") {
            self.params.mute.set(mute);
        }
        Ok(())
    }
}

impl DauxPlugin for Analyzer {
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

    /// `[main-thread]` Builds a GPUI editor reading this instance's spectrum.
    ///
    /// May be called any number of times while audio is running, and the result may be dropped
    /// at any moment: the editor's lifetime is independent of the processor's (`CLAUDE.md`
    /// rule 9). All a new editor takes is a clone of the `Arc`, and nothing it owns is
    /// reachable from `process`.
    ///
    /// Nothing GPUI-related is created here — [`GpuiEditor`] builds its instance in
    /// `DauxGraphic::open` — so an editor that the user never opens costs one allocation.
    fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
        let spectrum = Arc::clone(&self.spectrum);
        editor(GpuiEditor::new(EDITOR_SIZE, move |_window, cx| {
            let spectrum = Arc::clone(&spectrum);
            cx.new(|_| SpectrumView { spectrum })
        }))
    }

    fn accepts_bus_layout(&self, layout: &BusLayout) -> bool {
        let channels = |bus: Option<&BusInfo>| bus.map_or(0, BusInfo::channel_count);
        let inputs = channels(layout.main_input());
        let outputs = channels(layout.main_output());
        layout.outputs.len() == 1 && outputs > 0 && (inputs == 0 || inputs == outputs)
    }
}

/// The GPUI view: [`BAND_COUNT`] bars, redrawn every frame.
///
/// It owns nothing but a handle to the measurement. Everything it draws is derived from
/// [`Spectrum::band`], which is one atomic load per bar — so a repaint cannot be slower than
/// the display and cannot interfere with the audio thread at all.
///
/// `[main-thread]`
pub struct SpectrumView {
    /// The measurement the audio thread publishes into.
    spectrum: Arc<Spectrum>,
}

impl SpectrumView {
    /// `[main-thread]` Builds a view over `spectrum`.
    #[must_use]
    pub const fn new(spectrum: Arc<Spectrum>) -> Self {
        Self { spectrum }
    }

    /// `[main-thread]` Maps a level in dBFS onto `0.0..=1.0` of the plot's height.
    #[must_use]
    pub fn normalized(db: f32) -> f32 {
        if !db.is_finite() {
            return 0.0;
        }
        ((db - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0)
    }
}

impl Render for SpectrumView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // A spectrum is continuous, so the view asks for the next frame unconditionally rather
        // than waiting for a `notify` it will never get: the audio thread publishes into an
        // atomic and deliberately knows nothing about the editor. This is the whole of the
        // "repaint" machinery — no timer thread, no channel, no wakeup from `process`.
        window.request_animation_frame();

        let bars = (0..BAND_COUNT).map(|index| {
            let level = Self::normalized(self.spectrum.band(index));
            // A one-pixel floor so an empty band is still a visible baseline rather than a
            // gap the eye reads as a missing band.
            let height = (level * PLOT_HEIGHT).max(1.0);
            // Low bands cool, high bands warm: a cheap, readable colour ramp.
            let warmth = index as f32 / (BAND_COUNT - 1).max(1) as f32;
            let red = (0x38 as f32 + warmth * (0xf4 - 0x38) as f32) as u32;
            let green = (0xbd as f32 - warmth * (0xbd - 0x7a) as f32) as u32;
            let colour = (red << 16) | (green << 8) | 0x5e;

            div().w(px(14.0)).h(px(height)).rounded_sm().bg(rgb(colour))
        });

        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .p_3()
            .bg(rgb(0x0f1419))
            .text_color(rgb(0xd8dee9))
            .child("DAUx Analyzer")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .justify_center()
                    .gap_1()
                    .h(px(PLOT_HEIGHT))
                    .children(bars),
            )
    }
}

export_plugin!(SingleFactory<Analyzer>);

/// The allocation tripwire, installed only while this crate's tests are compiled.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: daux_plugin::CountingAllocator = daux_plugin::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use daux_plugin::daux_rt::{AllocGuard, counting_allocator_installed};
    use daux_plugin::{EventBuffer, downcast_editor};
    use std::sync::atomic::{AtomicBool, Ordering};

    const SAMPLE_RATE: f64 = 48_000.0;
    const BLOCK: usize = 512;

    fn config() -> ProcessConfig {
        ProcessConfig::new(SAMPLE_RATE, BLOCK as u32)
    }

    fn prepared() -> Analyzer {
        let mut analyzer = Analyzer::default();
        analyzer.prepare(&config()).expect("a valid config");
        analyzer
    }

    /// Runs `blocks` blocks of a sine at `hz` through the analyzer and returns the last block's
    /// left output channel.
    fn run_sine(analyzer: &mut Analyzer, hz: f64, amplitude: f32, blocks: usize) -> Vec<f32> {
        let mut phase = 0.0f64;
        let step = hz / SAMPLE_RATE;
        let mut last = Vec::new();
        for _ in 0..blocks {
            let mut input = AudioStorage::<f32>::new(2, BLOCK);
            for frame in 0..BLOCK {
                let value = (phase * core::f64::consts::TAU).sin() as f32 * amplitude;
                for channel in 0..2 {
                    input.channel_mut(channel).expect("a channel")[frame] = value;
                }
                phase += step;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
            let mut output = AudioStorage::<f32>::new(2, BLOCK);
            let cfg = config();
            let host = RtHostServices::null();
            let ctx = ProcessContext::new(BLOCK, &cfg, &host);
            {
                let inputs = [input.as_ref()];
                let mut outputs = [output.as_mut()];
                let mut buses = AudioBuses::new(&inputs, &mut outputs, BLOCK);
                let events = EventBuffer::with_capacity(1, 16);
                let mut sink = EventBuffer::with_capacity(1, 16);
                let mut ports = ProcessEvents::new(&events, &mut sink);
                assert_ne!(
                    analyzer.process(&ctx, &mut buses, &mut ports),
                    ProcessStatus::Error
                );
            }
            last = output.channel(0).expect("channel 0").to_vec();
        }
        last
    }

    /// The band whose centre is nearest `hz`.
    fn nearest_band(hz: f64) -> usize {
        (0..BAND_COUNT)
            .min_by(|a, b| {
                let da = (band_center_hz(*a).ln() - hz.ln()).abs();
                let db = (band_center_hz(*b).ln() - hz.ln()).abs();
                da.partial_cmp(&db).expect("finite frequencies")
            })
            .expect("the bank is not empty")
    }

    // ---- the band layout -------------------------------------------------------------------

    #[test]
    fn the_bands_are_log_spaced_across_the_audible_range() {
        assert!((band_center_hz(0) - LOWEST_HZ).abs() < 1e-9);
        assert!((band_center_hz(BAND_COUNT - 1) - HIGHEST_HZ).abs() < 1e-6);
        // Log spacing means a constant ratio between neighbours.
        let first = band_center_hz(1) / band_center_hz(0);
        for index in 1..BAND_COUNT - 1 {
            let ratio = band_center_hz(index + 1) / band_center_hz(index);
            assert!(
                (ratio - first).abs() < 1e-9,
                "band {index} breaks the constant ratio: {ratio} vs {first}"
            );
        }
        assert!(
            band_q() > 1.0,
            "constant-Q bands must be narrower than an octave"
        );
    }

    // ---- the measurement -------------------------------------------------------------------

    #[test]
    fn a_sine_lights_its_own_band_and_not_the_far_ones() {
        let mut analyzer = prepared();
        let tone = 1_000.0;
        run_sine(&mut analyzer, tone, 0.5, 8);

        let target = nearest_band(tone);
        let level = analyzer.spectrum().band(target);
        assert!(
            level > -20.0,
            "band {target} ({:.0} Hz) read {level} dB for a -6 dBFS tone at {tone} Hz",
            band_center_hz(target)
        );

        // Three bands away is more than an octave off and must be far quieter.
        for far in [target.saturating_sub(4), (target + 4).min(BAND_COUNT - 1)] {
            if far == target {
                continue;
            }
            assert!(
                analyzer.spectrum().band(far) < level - 12.0,
                "band {far} ({:.0} Hz) leaked: {} dB against {level} dB",
                band_center_hz(far),
                analyzer.spectrum().band(far)
            );
        }
    }

    #[test]
    fn moving_the_tone_moves_the_peak() {
        for tone in [80.0, 500.0, 4_000.0] {
            let mut analyzer = prepared();
            run_sine(&mut analyzer, tone, 0.5, 8);
            let loudest = (0..BAND_COUNT)
                .max_by(|a, b| {
                    analyzer
                        .spectrum()
                        .band(*a)
                        .partial_cmp(&analyzer.spectrum().band(*b))
                        .expect("finite levels")
                })
                .expect("the bank is not empty");
            let expected = nearest_band(tone);
            assert!(
                loudest.abs_diff(expected) <= 1,
                "a {tone} Hz tone peaked in band {loudest} ({:.0} Hz), expected {expected} \
                 ({:.0} Hz)",
                band_center_hz(loudest),
                band_center_hz(expected)
            );
        }
    }

    #[test]
    fn silence_reads_as_silence() {
        let analyzer = prepared();
        for band in 0..BAND_COUNT {
            assert_eq!(analyzer.spectrum().band(band), SILENCE_DB);
        }
    }

    #[test]
    fn the_bands_fall_back_after_the_tone_stops() {
        let mut analyzer = prepared();
        analyzer.params.release.set_plain(10.0);
        analyzer.prepare(&config()).expect("a valid config");
        run_sine(&mut analyzer, 1_000.0, 0.5, 8);
        let target = nearest_band(1_000.0);
        let loud = analyzer.spectrum().band(target);

        // Half a second of silence, at a 10 ms release.
        run_sine(&mut analyzer, 1_000.0, 0.0, 48);
        let quiet = analyzer.spectrum().band(target);
        assert!(
            quiet < loud - 30.0,
            "the band did not fall back: {loud} dB then {quiet} dB"
        );
    }

    #[test]
    fn reset_clears_the_display_as_well_as_the_filters() {
        let mut analyzer = prepared();
        run_sine(&mut analyzer, 1_000.0, 0.5, 4);
        assert!(analyzer.spectrum().band(nearest_band(1_000.0)) > SILENCE_DB);
        analyzer.reset();
        for band in 0..BAND_COUNT {
            assert_eq!(analyzer.spectrum().band(band), SILENCE_DB);
        }
    }

    // ---- the audio path --------------------------------------------------------------------

    #[test]
    fn the_audio_passes_through_untouched() {
        let mut analyzer = prepared();
        let out = run_sine(&mut analyzer, 1_000.0, 0.5, 1);
        let peak = out.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        assert!(
            (peak - 0.5).abs() < 1e-4,
            "an analyzer must not change the audio: peak {peak}"
        );
    }

    #[test]
    fn mute_silences_the_output_but_keeps_measuring() {
        let mut analyzer = prepared();
        analyzer.params.mute.set(true);
        let out = run_sine(&mut analyzer, 1_000.0, 0.5, 4);
        assert!(
            out.iter().all(|s| *s == 0.0),
            "mute must silence the output"
        );
        assert!(
            analyzer.spectrum().band(nearest_band(1_000.0)) > -20.0,
            "mute must not stop the measurement"
        );
    }

    #[test]
    fn process_never_allocates() {
        assert!(
            counting_allocator_installed(),
            "the tripwire is not installed, so this test would pass vacuously"
        );
        let mut analyzer = prepared();
        // Warm the filters up so the measured block is a steady-state one.
        run_sine(&mut analyzer, 1_000.0, 0.5, 2);
        // A release change forces the follower retiming path to run inside the measured block.
        analyzer.params.release.set_plain(500.0);

        let mut input = AudioStorage::<f32>::new(2, BLOCK);
        input.fill(0.25);
        let mut output = AudioStorage::<f32>::new(2, BLOCK);
        let cfg = config();
        let host = RtHostServices::null();
        let ctx = ProcessContext::new(BLOCK, &cfg, &host);
        let inputs = [input.as_ref()];
        let mut outputs = [output.as_mut()];
        let mut buses = AudioBuses::new(&inputs, &mut outputs, BLOCK);
        let events = EventBuffer::with_capacity(1, 16);
        let mut sink = EventBuffer::with_capacity(1, 16);
        let mut ports = ProcessEvents::new(&events, &mut sink);

        let (_, allocations) = AllocGuard::scope(|| analyzer.process(&ctx, &mut buses, &mut ports));
        assert_eq!(allocations, 0, "process allocated {allocations} time(s)");
    }

    #[test]
    fn a_block_larger_than_the_prepared_maximum_is_clamped_rather_than_grown() {
        let mut analyzer = prepared();
        let capacity = analyzer.mono.capacity();
        let frames = BLOCK * 2;
        let mut input = AudioStorage::<f32>::new(2, frames);
        input.fill(0.5);
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
            analyzer.process(&ctx, &mut buses, &mut ports);
        }
        assert_eq!(
            analyzer.mono.capacity(),
            capacity,
            "process re-allocated the mono buffer"
        );
    }

    #[test]
    fn prepare_refuses_a_configuration_it_cannot_size_from() {
        let mut analyzer = Analyzer::default();
        assert!(
            analyzer
                .prepare(&ProcessConfig::new(f64::NAN, 512))
                .is_err()
        );
        assert!(analyzer.prepare(&ProcessConfig::new(48_000.0, 0)).is_err());
        assert!(analyzer.bands.is_empty());
    }

    #[test]
    fn a_low_sample_rate_does_not_produce_bands_above_nyquist() {
        let mut analyzer = Analyzer::default();
        analyzer
            .prepare(&ProcessConfig::new(8_000.0, 128))
            .expect("8 kHz is a valid rate");
        // Every filter must still be usable: a NaN coefficient would poison the whole mix.
        run_sine(&mut analyzer, 1_000.0, 0.5, 1);
        for band in 0..BAND_COUNT {
            assert!(
                analyzer.spectrum().band(band).is_finite(),
                "band {band} produced a non-finite level at 8 kHz"
            );
        }
    }

    // ---- the cross-thread contract ---------------------------------------------------------

    #[test]
    fn the_spectrum_crosses_threads_without_a_lock() {
        // The property that matters is not "it compiles" but "a reader running concurrently
        // with the writer never blocks and never sees a torn value".
        let spectrum = Arc::new(Spectrum::new());
        let stop = Arc::new(AtomicBool::new(false));

        let reader = {
            let spectrum = Arc::clone(&spectrum);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut seen_loud = false;
                while !stop.load(Ordering::Relaxed) {
                    for band in 0..BAND_COUNT {
                        let value = spectrum.band(band);
                        assert!(value.is_finite(), "a torn read produced {value}");
                        if value > -1.0 {
                            seen_loud = true;
                        }
                    }
                }
                seen_loud
            })
        };

        for _ in 0..5_000 {
            for band in 0..BAND_COUNT {
                spectrum.publish(band, 0.0);
            }
            for band in 0..BAND_COUNT {
                spectrum.publish(band, SILENCE_DB);
            }
        }
        stop.store(true, Ordering::Relaxed);
        assert!(
            reader.join().expect("the reader must not panic"),
            "the reader never observed a published value"
        );
    }

    #[test]
    fn publishing_never_allocates_and_an_unknown_band_is_ignored() {
        assert!(counting_allocator_installed());
        let spectrum = Spectrum::new();
        let (_, allocations) = AllocGuard::scope(|| {
            for band in 0..BAND_COUNT {
                spectrum.publish(band, -12.0);
            }
            // Out of range: ignored rather than panicking on the audio thread.
            spectrum.publish(BAND_COUNT, 0.0);
            spectrum.publish(usize::MAX, 0.0);
            spectrum.clear();
        });
        assert_eq!(allocations, 0);
        assert_eq!(spectrum.band(BAND_COUNT), SILENCE_DB);
        assert_eq!(spectrum.band(0), SILENCE_DB);
    }

    // ---- the editor --------------------------------------------------------------------------

    #[test]
    fn every_editor_reads_the_same_measurement() {
        // Rule 9: an editor may be created and dropped many times while the DSP keeps running,
        // and each one must see the live spectrum rather than a snapshot.
        let mut analyzer = prepared();
        run_sine(&mut analyzer, 1_000.0, 0.5, 4);
        let live = analyzer.spectrum().band(nearest_band(1_000.0));

        for _ in 0..3 {
            let boxed = analyzer
                .create_editor()
                .expect("this plug-in has an editor");
            let editor = downcast_editor(boxed).expect("wrapped with `editor(..)`");
            assert_eq!(editor.descriptor().preferred_size, EDITOR_SIZE);
            // Dropped without ever being opened: an editor holds no GPU resources until
            // `open`, so this costs one allocation and touches no DSP state.
            drop(editor);
        }

        // The DSP is untouched by all that.
        assert_eq!(analyzer.spectrum().band(nearest_band(1_000.0)), live);
        run_sine(&mut analyzer, 1_000.0, 0.5, 1);
        assert!(analyzer.spectrum().band(nearest_band(1_000.0)) > -20.0);
    }

    #[test]
    fn the_view_maps_levels_onto_the_plot_the_way_the_scale_says() {
        assert_eq!(SpectrumView::normalized(CEILING_DB), 1.0);
        assert_eq!(SpectrumView::normalized(FLOOR_DB), 0.0);
        assert_eq!(SpectrumView::normalized(SILENCE_DB), 0.0, "below the floor");
        assert_eq!(SpectrumView::normalized(12.0), 1.0, "above the ceiling");
        let middle = SpectrumView::normalized((FLOOR_DB + CEILING_DB) * 0.5);
        assert!((middle - 0.5).abs() < 1e-6, "the scale is not linear in dB");
        // A follower that has never seen audio reports `-inf` on some paths; the view must
        // not turn that into a `NaN` rectangle that swallows every later hit test.
        assert_eq!(SpectrumView::normalized(f32::NEG_INFINITY), 0.0);
        assert_eq!(SpectrumView::normalized(f32::NAN), 0.0);
    }

    // ---- metadata ----------------------------------------------------------------------------

    #[test]
    fn the_descriptor_says_it_analyses_and_has_an_editor() {
        let d = <Analyzer as DauxPlugin>::descriptor();
        d.validate().expect("the descriptor must be valid");
        assert_eq!(d.category, Category::Analyzer);
        assert!(d.capabilities.is_analyzer());
        assert!(d.capabilities.is_has_gui());
        assert!(
            !d.capabilities.is_requires_gui(),
            "the measurement works headless; only the display needs a window"
        );
    }

    #[test]
    fn state_round_trips_and_leaves_the_measurement_out_of_it() {
        let analyzer = Analyzer::default();
        analyzer.params.release.set_plain(1_500.0);
        analyzer.params.mute.set(true);
        analyzer.spectrum.publish(0, 0.0);

        let mut writer = StateWriter::new(StateVersion(STATE_VERSION));
        analyzer
            .save_state(&mut writer)
            .expect("saving cannot fail");
        let blob = writer.finish();

        let mut restored = Analyzer::default();
        let reader = StateReader::from_bytes(&blob).expect("the blob we wrote parses");
        restored.load_state(&reader).expect("loading cannot fail");
        assert!((restored.params.release.value() - 1_500.0).abs() < 1e-9);
        assert!(restored.params.mute.value());
        assert_eq!(
            restored.spectrum().band(0),
            SILENCE_DB,
            "a measurement of past audio is not state"
        );
    }

    #[test]
    fn every_parameter_is_reachable_by_its_permanent_id() {
        let params = AnalyzerParams::new();
        assert_eq!(params.param_refs().len(), 2);
        assert!(params.param(ParamId::new(param_id::RELEASE)).is_some());
        assert!(params.param(ParamId::new(param_id::MUTE)).is_some());
        assert!(params.param(ParamId::new(99)).is_none());
    }
}
