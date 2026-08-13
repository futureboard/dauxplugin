# Crate contracts

This document fixes the **cross-crate public surface** of the DAUxPlug workspace. It is
the coordination contract: a crate may design its internals freely, but the items listed
here are what other crates compile against, so they must exist with these names and
shapes.

Rules that apply everywhere:

* No crate may add a dependency that is not already in its `Cargo.toml`. The dependency
  graph in the root `Cargo.toml` is deliberate and acyclic.
* `daux-abi`, `daux-rt`, `daux-audio`, `daux-midi`, `daux-events`, `daux-parameter`,
  `daux-state`, `daux-transport`, `daux-core` have **zero external dependencies**. This is
  a hard architectural rule, not a preference.
* Everything public is documented (`missing_docs` is warned on) and every `unsafe` block
  carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks`).
* Types crossing the audio thread are `Send`; types shared with the UI are `Sync`.
* Anything that can be called on the audio thread is annotated in its doc comment with
  `[audio-thread]`, `[main-thread]` or `[any-thread]`, matching `docs/specifications/abi-v1.md` §15.

---

## `daux-abi`

A literal transcription of `docs/specifications/abi-v1.md`. Constants, `#[repr(C)]`
structs, `unsafe extern "C"` function-pointer types, nothing else. No logic beyond small
`const fn`/inline helpers for the fixed text buffers and status conversion.

```rust
pub struct DauxStatus(pub i32);                 // + all DAUX_* constants from the spec
pub type DauxBool = u32;
pub struct DauxStrView { pub ptr: *const u8, pub len: usize }
pub struct DauxVersion { major, minor, patch, build: u32 }
pub struct DauxName(pub [u8; 64]);   pub struct DauxText(pub [u8; 256]);
pub struct DauxId(pub [u8; 128]);    pub struct DauxPath(pub [u8; 1024]);

impl DauxStrView { pub const fn empty() -> Self; pub fn from_str(s: &str) -> Self;
                   pub unsafe fn as_str<'a>(self) -> Option<&'a str>; }
impl DauxName    { pub fn new(s: &str) -> Self;  // truncates on a char boundary
                   pub fn as_str(&self) -> &str; pub fn set(&mut self, s: &str); }
// identical inherent API on DauxText / DauxId / DauxPath

pub struct DauxFactoryHandle(pub *mut c_void);  // + DauxPluginHandle, DauxHostHandle
pub struct DauxFactoryV1 { pub handle: DauxFactoryHandle, pub api: *const DauxFactoryApiV1 }
pub struct DauxPluginV1  { .. }   pub struct DauxHostV1 { .. }

pub struct DauxPluginEntryV1 { .. }        pub struct DauxFactoryApiV1 { .. }
pub struct DauxPluginApiV1 { .. }          pub struct DauxPluginDescriptorV1 { .. }
pub struct DauxProcessConfigV1 { .. }      pub struct DauxProcessV1 { .. }
pub struct DauxAudioBufferV1 { .. }        pub struct DauxTransportV1 { .. }
pub struct DauxEventHeaderV1 { .. }        pub struct DauxEventListV1 { .. }
pub struct DauxEventNoteV1 { .. }          pub struct DauxEventNoteExpressionV1 { .. }
pub struct DauxEventParamV1 { .. }         pub struct DauxEventMidi1V1 { .. }
pub struct DauxEventMidi2V1 { .. }         pub struct DauxEventSysExV1 { .. }
pub struct DauxAudioPortsApiV1 { .. }      pub struct DauxAudioPortInfoV1 { .. }
pub struct DauxParamsApiV1 { .. }          pub struct DauxParamInfoV1 { .. }
pub struct DauxStateApiV1 { .. }           pub struct DauxStreamV1 { .. }
pub struct DauxGuiApiV1 { .. }             pub struct DauxWindowV1 { .. }
pub struct DauxLatencyApiV1 { .. }         pub struct DauxTailApiV1 { .. }
pub struct DauxRenderApiV1 { .. }          pub struct DauxHostApiV1 { .. }
pub struct DauxHostLogApiV1 { .. }         pub struct DauxHostParamsApiV1 { .. }
pub struct DauxHostWorkerApiV1 { .. }      pub struct DauxHostGuiApiV1 { .. }
pub struct DauxSharedTextureV1 { .. }

pub mod ext {  // extension id string constants
    pub const AUDIO_PORTS: &str = "daux.audio-ports/1";  // etc.
}
```

Every struct: `#[repr(C)]`, `Debug`, `Clone`, `Copy` where sound, and a
`pub const fn empty()`/`Default` that zeroes reserved fields and sets `size`.
Provide `pub const fn size_of_v1_0<T>()` style minimum-size constants used for
validation, and a `impl DauxProcessV1 { pub fn field_present(&self, offset, width) }`
helper or equivalent used by readers.

`daux-abi` must compile with `#![no_std]` + `extern crate core` semantics (it may still
be a normal `std` crate, but must not reference `std::` items other than through `core`).

---

## `daux-rt`

Real-time primitives. No allocation outside explicit constructors; every constructor
documents that it allocates and is `[main-thread]`.

```rust
pub struct SpscRingBuffer;                     // factory
impl SpscRingBuffer { pub fn with_capacity<T: Send>(cap: usize) -> (Producer<T>, Consumer<T>); }
pub struct Producer<T> { .. }   // push(&mut self, T) -> Result<(), Full<T>>   [audio-thread]
pub struct Consumer<T> { .. }   // pop(&mut self) -> Option<T>, len(), is_empty()

pub struct MpscQueue<T>;        // bounded, lock-free, many producers
impl<T: Send> MpscQueue<T> { pub fn with_capacity(cap: usize) -> Self;
    pub fn try_push(&self, v: T) -> Result<(), Full<T>>; pub fn pop(&self) -> Option<T>; }

pub struct TripleBuffer<T: Clone + Send>;   // audio → UI snapshots, never blocks
impl<T: Clone + Send + Default> TripleBuffer<T> {
    pub fn new(initial: T) -> (TripleWriter<T>, TripleReader<T>); }
pub struct TripleWriter<T> { pub fn write(&mut self, v: T);  pub fn with(&mut self, f: impl FnOnce(&mut T)); }
pub struct TripleReader<T> { pub fn read(&mut self) -> &T; pub fn has_update(&self) -> bool; }

pub struct AtomicF32(..);  pub struct AtomicF64(..);   // relaxed/acquire-release helpers
impl AtomicF32 { pub fn new(v: f32) -> Self; pub fn get(&self) -> f32; pub fn set(&self, v: f32); }

pub struct FixedVec<T> { .. }   // bounded Vec: with_capacity, push -> Result, clear,
                                // as_slice, as_mut_slice, Deref<[T]>, iter, len, capacity
pub struct ScratchBuffers<T>;   // preallocated per-channel scratch
impl<T: Copy + Default> ScratchBuffers<T> {
    pub fn new(channels: usize, frames: usize) -> Self;
    pub fn channel_mut(&mut self, i: usize) -> &mut [T];    // [audio-thread]
    pub fn slice_mut(&mut self, i: usize, frames: usize) -> &mut [T]; }

pub struct RtLogQueue;  // bounded records, formatting happens on the consumer side
pub struct RtLogRecord { pub level: LogLevel, pub len: u8, pub bytes: [u8; 120] }
pub enum LogLevel { Trace, Debug, Info, Warn, Error, Fatal }

pub enum ThreadClass { Main, Audio, Ui, Worker, Scanner, Ipc, Unknown }
pub fn current_thread_class() -> ThreadClass;
pub fn set_current_thread_class(c: ThreadClass);
/// Debug-only assertion; compiles away in release.
#[macro_export] macro_rules! rt_assert_audio_thread { .. }
/// Debug-only allocation tripwire used by tests: counts allocations in a scope.
pub struct AllocGuard;  impl AllocGuard { pub fn scope<R>(f: impl FnOnce() -> R) -> (R, usize); }
```

`AllocGuard` is implemented with a `#[global_allocator]` shim that is only installed by
the test harness (`daux-rt` exposes `pub struct CountingAllocator` + `pub fn alloc_count()`);
it must not affect production builds.

---

## `daux-audio`

```rust
pub trait Sample: Copy + Send + Sync + 'static {
    const ZERO: Self; const FORMAT: SampleFormat;
    fn from_f64(v: f64) -> Self; fn to_f64(self) -> f64;
    fn from_f32(v: f32) -> Self; fn to_f32(self) -> f32;
}                                    // implemented for f32 and f64 only (sealed)

pub enum SampleFormat { F32, F64 }   // as_bits()/from_bits() ↔ DAUX_SAMPLE_FORMAT_*
pub enum ChannelLayout { Mono, Stereo, LRC, Quad, Surround2_1, Surround5_1, Surround7_1,
                         Atmos7_1_4, Ambisonic1st, Ambisonic2nd, Ambisonic3rd,
                         Discrete(u16), Custom(u16) }
impl ChannelLayout { pub fn channel_count(self) -> u16; pub fn as_bits(self) -> u32;
                     pub fn from_bits(bits: u32, channels: u16) -> Self; }
pub enum BusPurpose { Main, Aux, Sidechain, Monitor, Analysis, Reference, Cv, Control }
pub struct BusFlags(u32);   // IS_MAIN | OPTIONAL | CV | SUPPORTS_64
pub struct BusInfo { pub id: u32, pub name: String, pub layout: ChannelLayout,
                     pub purpose: BusPurpose, pub flags: BusFlags }
pub struct BusLayout { pub inputs: Vec<BusInfo>, pub outputs: Vec<BusInfo> }

pub struct AudioBufferRef<'a, T: Sample> { .. }
impl<'a, T: Sample> AudioBufferRef<'a, T> {
    pub unsafe fn from_raw(ptrs: *const *const T, channels: usize, frames: usize) -> Self;
    pub fn channel_count(&self) -> usize;  pub fn frames(&self) -> usize;
    pub fn channel(&self, i: usize) -> &'a [T];
    pub fn iter(&self) -> impl Iterator<Item = &'a [T]>;
    pub fn is_channel_constant(&self, i: usize) -> bool; }

pub struct AudioBufferMut<'a, T: Sample> { .. }   // same + channel_mut, split_channels_mut,
                                                  // fill_silence, copy_from
pub struct AudioBuses<'a, T: Sample> { .. }       // inputs()/outputs() → indexed buses
impl<'a, T: Sample> AudioBuses<'a, T> {
    pub fn input(&self, bus: usize) -> Option<AudioBufferRef<'a, T>>;
    pub fn output(&mut self, bus: usize) -> Option<AudioBufferMut<'_, T>>;
    pub fn main_input(&self) -> Option<AudioBufferRef<'a, T>>;
    pub fn main_output(&mut self) -> Option<AudioBufferMut<'_, T>>;
    pub fn input_count(&self) -> usize; pub fn output_count(&self) -> usize;
    pub fn frames(&self) -> usize; }

/// Owned, preallocated storage used by hosts, tests and offline rendering.
pub struct AudioStorage<T: Sample> { pub fn new(channels: usize, frames: usize) -> Self;
    pub fn as_ref(&self) -> AudioBufferRef<'_, T>; pub fn as_mut(&mut self) -> AudioBufferMut<'_, T>; }
```

Zero-copy is mandatory: no method in this crate may allocate except `AudioStorage::new`
and `BusLayout` construction.

---

## `daux-midi`

```rust
pub struct Midi1Message { pub bytes: [u8; 3] }
impl Midi1Message { pub fn status(&self) -> u8; pub fn channel(&self) -> u8;
    pub fn kind(&self) -> Midi1Kind; pub fn note_on(ch, key, vel) -> Self; /* … */ }
pub enum Midi1Kind { NoteOff, NoteOn, PolyPressure, ControlChange, ProgramChange,
                     ChannelPressure, PitchBend, System }

/// MIDI 2.0 Universal MIDI Packet: 1–4 32-bit words.
pub struct Ump { pub words: [u32; 4], pub len: u8 }
impl Ump { pub fn message_type(&self) -> u8; pub fn group(&self) -> u8;
           pub fn as_words(&self) -> &[u32]; }
pub enum Midi2Message { NoteOn { group, channel, note, velocity: u16, attribute },
                        NoteOff { .. }, ControlChange { .. }, PitchBend { .. },
                        PerNotePitchBend { .. }, RegisteredController { .. },
                        AssignableController { .. }, ProgramChange { .. }, Other(Ump) }
impl Midi2Message { pub fn to_ump(&self) -> Ump; pub fn from_ump(u: Ump) -> Option<Self>; }

pub fn midi1_to_midi2(m: Midi1Message, group: u8) -> Option<Midi2Message>;
pub fn midi2_to_midi1(m: &Midi2Message) -> Option<Midi1Message>;   // lossy, documented

pub struct SysEx7<'a>(pub &'a [u8]);   // borrowed, no allocation
```

---

## `daux-events`

```rust
pub struct EventHeader { pub time: u32, pub port_index: u16, pub flags: EventFlags }
pub struct EventFlags(u16);   // IS_LIVE | DONT_RECORD

pub enum DauxEvent<'a> {
    NoteOn(NoteEvent), NoteOff(NoteEvent), NoteChoke(NoteEvent), NoteEnd(NoteEvent),
    NoteExpression(NoteExpressionEvent),
    ParamValue(ParamEvent), ParamMod(ParamEvent),
    ParamGestureBegin(ParamGestureEvent), ParamGestureEnd(ParamGestureEvent),
    Transport(TransportEvent),
    Midi1(Midi1Event), Midi2(Midi2Event), SysEx(SysExEvent<'a>),
    Custom(CustomEvent<'a>),
}
impl<'a> DauxEvent<'a> { pub fn header(&self) -> EventHeader; pub fn time(&self) -> u32;
                         pub fn kind_bits(&self) -> u16; }

pub struct NoteEvent { pub header: EventHeader, pub note_id: i32, pub channel: i16,
                       pub key: i16, pub velocity: f64, pub tuning: f64 }
pub struct NoteExpressionEvent { pub header, pub expression: NoteExpression,
                                 pub note_id: i32, pub channel: i16, pub key: i16, pub value: f64 }
pub enum NoteExpression { Volume, Pan, Tuning, Vibrato, Expression, Brightness, Pressure }
pub struct ParamEvent { pub header, pub param_id: u32, pub note_id: i32,
                        pub channel: i16, pub key: i16, pub value: f64 }
pub struct ParamGestureEvent { pub header, pub param_id: u32 }
pub struct TransportEvent { pub header, pub transport: /* daux-transport is NOT a dep */
                            TransportSnapshot }
pub struct SysExEvent<'a> { pub header, pub bytes: &'a [u8] }
pub struct CustomEvent<'a> { pub header, pub kind: u16, pub bytes: &'a [u8] }

/// Read-only, borrowed, sorted-by-time event input for one block. [audio-thread]
pub trait InputEvents { fn len(&self) -> usize;
                        fn get(&self, index: usize) -> Option<DauxEvent<'_>>;
                        fn iter(&self) -> InputEventIter<'_> where Self: Sized; }
/// Bounded event output. `push` never allocates and may legitimately fail. [audio-thread]
pub trait OutputEvents { fn try_push(&mut self, e: &DauxEvent<'_>) -> Result<(), EventOverflow>; }

/// Owned bounded storage implementing both traits, used by hosts and tests.
pub struct EventBuffer { pub fn with_capacity(events: usize, bytes: usize) -> Self;
                         pub fn clear(&mut self); pub fn sort_by_time(&mut self); }
pub struct EventOverflow;
```

`TransportSnapshot` lives in `daux-events` as a plain `#[derive(Clone, Copy)]` mirror of
the transport fields (`daux-events` must not depend on `daux-transport`);
`daux-transport::Transport` provides `From`/`Into` conversions.

---

## `daux-parameter`

```rust
pub struct ParamId(pub u32);
pub struct ParamFlags(u32);   // AUTOMATABLE | MODULATABLE | PER_NOTE | STEPPED |
                              // READ_ONLY | HIDDEN | BYPASS | REQUIRES_PROCESS | IS_METER
pub struct ParamInfo { pub id: ParamId, pub name: String, pub group: String,
                       pub unit: String, pub flags: ParamFlags, pub step_count: u32,
                       pub min: f64, pub max: f64, pub default: f64 }

pub enum ParamRange { Linear { min: f64, max: f64 },
                      Skewed { min: f64, max: f64, factor: f64 },
                      Logarithmic { min: f64, max: f64 },
                      Stepped { min: i64, max: i64 },
                      Boolean }
impl ParamRange { pub fn normalize(&self, plain: f64) -> f64;      // → 0..=1
                  pub fn denormalize(&self, norm: f64) -> f64;
                  pub fn clamp(&self, plain: f64) -> f64; }

/// Object-safe: every concrete parameter type implements it. Values are plain
/// (real-world) units; normalisation stays inside the plug-in.
pub trait Param: Send + Sync {
    fn info(&self) -> ParamInfo;
    fn plain(&self) -> f64;                       // [any-thread]
    fn set_plain(&self, v: f64);                  // [any-thread], atomic
    fn normalized(&self) -> f64;
    fn set_normalized(&self, v: f64);
    fn to_text(&self, plain: f64, out: &mut String);
    fn from_text(&self, text: &str) -> Option<f64>;
    fn reset(&self);
}

pub struct FloatParam { .. }  // new(id, name, default, range) + .with_unit(..)
                              // .with_smoothing(Smoothing) .with_flags(..) .with_formatter(..)
pub struct IntParam { .. }    pub struct BoolParam { .. }
pub struct EnumParam<E: ParamEnum> { .. }         pub struct MeterParam { .. }
pub trait ParamEnum: Copy + 'static { const VARIANTS: &'static [Self]; 
                                      fn name(self) -> &'static str; fn index(self) -> u32;
                                      fn from_index(i: u32) -> Option<Self>; }

/// Implemented by hand or via `#[derive(DauxParams)]`.
pub trait Params: Send + Sync {
    fn param_refs(&self) -> Vec<(ParamId, &dyn Param)>;   // [main-thread], stable order
    fn param(&self, id: ParamId) -> Option<&dyn Param>;   // [any-thread]
    fn state_schema_version(&self) -> u32 { 1 }
}

pub enum Smoothing { None, Linear { ms: f32 }, Exponential { ms: f32 } }
pub struct Smoother { pub fn new(smoothing: Smoothing) -> Self;
                      pub fn prepare(&mut self, sample_rate: f64);          // [main-thread]
                      pub fn set_target(&mut self, v: f32);                 // [audio-thread]
                      pub fn next(&mut self) -> f32;                        // [audio-thread]
                      pub fn next_block(&mut self, out: &mut [f32]);
                      pub fn is_smoothing(&self) -> bool; pub fn reset_to(&mut self, v: f32); }

/// Renames/removals across plug-in versions.
pub struct ParamMigration { pub fn rename(old: ParamId, new: ParamId) -> Self;
                            pub fn removed(id: ParamId) -> Self; }
```

`FloatParam` and friends store their value in a `daux_rt::AtomicF32`/`AtomicF64` so that
`&P` is `Sync` and the same `Arc<Params>` is shared by the processor, controller and editor.

---

## `daux-state`

```rust
pub struct StateVersion(pub u32);
pub struct StateError { .. }   // Display + std::error::Error; kinds: Io, Corrupt,
                               // UnsupportedVersion{found,supported}, MissingField, Migration
pub type StateResult<T> = Result<T, StateError>;

/// Deterministic tagged binary container: magic "DAUXST\0\0", u32 version,
/// then length-prefixed (key, type, value) entries in insertion order.
pub struct StateWriter { pub fn new(version: StateVersion) -> Self;
    pub fn put_f64(&mut self, key: &str, v: f64); pub fn put_i64(..); pub fn put_bool(..);
    pub fn put_str(&mut self, key: &str, v: &str); pub fn put_bytes(&mut self, key: &str, v: &[u8]);
    pub fn begin_group(&mut self, key: &str); pub fn end_group(&mut self);
    pub fn finish(self) -> Vec<u8>;
    pub fn write_to(self, w: &mut dyn std::io::Write) -> StateResult<()>; }

pub struct StateReader { pub fn from_bytes(b: &[u8]) -> StateResult<Self>;
    pub fn read_from(r: &mut dyn std::io::Read) -> StateResult<Self>;
    pub fn version(&self) -> StateVersion;
    pub fn f64(&self, key: &str) -> StateResult<f64>; /* i64, bool, str, bytes, group */
    pub fn opt_f64(&self, key: &str) -> Option<f64>; }

/// Chain of `from → from+1` steps applied in order until the current version is reached.
pub struct MigrationChain { pub fn new(current: StateVersion) -> Self;
    pub fn step(self, from: StateVersion, f: fn(&mut StateDoc) -> StateResult<()>) -> Self;
    pub fn migrate(&self, doc: StateDoc) -> StateResult<StateDoc>; }
pub struct StateDoc { .. }   // mutable in-memory form used by migrations
```

Every read is bounds-checked; a truncated or hostile blob returns `StateError`, never a
panic and never an unbounded allocation (enforce a configurable max blob size, default 64 MiB).

---

## `daux-transport`

```rust
pub struct TransportFlags(u32);   // mirrors DAUX_TRANSPORT_*
pub struct TimeSignature { pub numerator: u16, pub denominator: u16 }
pub struct Transport { pub flags: TransportFlags, pub song_pos_samples: i64,
                       pub song_pos_beats: f64, pub song_pos_seconds: f64,
                       pub tempo: f64, pub tempo_increment: f64,
                       pub bar_start_beats: f64, pub bar_number: i32,
                       pub time_signature: TimeSignature,
                       pub loop_start_beats: f64, pub loop_end_beats: f64,
                       pub loop_start_seconds: f64, pub loop_end_seconds: f64 }
impl Transport {
    pub fn is_playing(&self) -> bool;  pub fn is_recording(&self) -> bool;
    pub fn is_looping(&self) -> bool;
    pub fn tempo(&self) -> Option<f64>;            // None unless HAS_TEMPO
    pub fn beats(&self) -> Option<f64>; pub fn seconds(&self) -> Option<f64>;
    pub fn time_signature(&self) -> Option<TimeSignature>;
    pub fn loop_range_beats(&self) -> Option<(f64, f64)>;
    pub fn beats_to_samples(&self, beats: f64, sample_rate: f64) -> Option<f64>;
    pub fn samples_to_beats(&self, samples: f64, sample_rate: f64) -> Option<f64>; }
```

Accessors return `Option` precisely so a plug-in cannot read a field the host never set.

---

## `daux-dsp`

Deliberately small. Only things every second plug-in needs, plus SIMD dispatch.

```rust
pub fn db_to_gain(db: f32) -> f32;   pub fn gain_to_db(gain: f32) -> f32;
pub fn db_to_gain_f64(db: f64) -> f64;  pub fn gain_to_db_f64(..) -> f64;

pub struct Biquad { pub fn lowpass(sr, freq, q) -> Self; /* highpass, bandpass, notch,
     peak, lowshelf, highshelf, allpass */ pub fn process(&mut self, x: f32) -> f32;
     pub fn process_block(&mut self, buf: &mut [f32]); pub fn reset(&mut self); }
pub struct OnePole { .. }   pub struct DcBlocker { .. }
pub struct PeakFollower { .. }  // meters: attack/release in ms
pub struct DelayLine { pub fn new(max_samples: usize) -> Self; /* [main-thread] */
                       pub fn read(&self, delay: f32) -> f32; pub fn write(&mut self, x: f32); }

/// Runtime-dispatched vector helpers; always correct, never required.
pub mod simd {
    pub fn apply_gain(buf: &mut [f32], gain: f32);
    pub fn apply_gain_ramp(buf: &mut [f32], from: f32, to: f32);
    pub fn add_from(dst: &mut [f32], src: &[f32]);
    pub fn copy_from(dst: &mut [f32], src: &[f32]);
    pub fn peak_abs(buf: &[f32]) -> f32;
    pub fn dispatch_name() -> &'static str;   // "scalar" | "sse2" | "avx2" | "neon"
}
```

SIMD paths use `is_x86_feature_detected!` (cached in a `OnceLock`) and must have a scalar
fallback that produces bit-identical results for the operations above, or the difference
must be documented. Binaries must never fault on a CPU without the feature.

---

## `daux-host-services`

```rust
pub trait HostLog: Send + Sync { fn log(&self, level: LogLevel, msg: &str); }   // [any-thread]
pub trait HostParams: Send + Sync {
    fn gesture_begin(&self, id: ParamId); fn gesture_end(&self, id: ParamId);
    fn changed(&self, id: ParamId, plain: f64); fn rescan(&self, flags: RescanFlags); }
pub trait HostLatency: Send + Sync { fn set_samples(&self, samples: u32); }
pub trait HostTail: Send + Sync { fn changed(&self); }
pub trait HostWorker: Send + Sync { fn schedule(&self, task: TaskId) -> bool; }  // [audio-thread] ok
pub trait HostGui: Send + Sync { fn request_resize(&self, w: u32, h: u32) -> bool;
                                 fn request_show(&self) -> bool; fn closed(&self, destroyed: bool); }
pub trait HostTimer: Send + Sync { fn register(&self, period_ms: u32) -> Option<TimerId>;
                                   fn unregister(&self, id: TimerId); }
pub trait HostResources: Send + Sync {              // bundle-relative, [main-thread]
    fn read(&self, logical_path: &str) -> std::io::Result<Vec<u8>>;
    fn read_to_string(&self, logical_path: &str) -> std::io::Result<String>;
    fn exists(&self, logical_path: &str) -> bool; }
pub trait ThreadCheck: Send + Sync { fn is_main_thread(&self) -> bool;
                                     fn is_audio_thread(&self) -> bool; }

pub struct HostInfo { pub name: String, pub vendor: String, pub version: String }

/// Everything a plug-in may reach on a non-real-time thread. Optional services are
/// `None` when the host does not provide them — plug-ins MUST degrade gracefully.
pub struct HostServices { pub fn info(&self) -> &HostInfo;
    pub fn log(&self) -> &dyn HostLog;                  // always present (no-op fallback)
    pub fn params(&self) -> Option<&dyn HostParams>;
    pub fn latency(&self) -> Option<&dyn HostLatency>;  /* tail, worker, gui, timer,
                                                           resources, threads */ }

/// The strictly real-time-safe subset handed to `process`. Every method here is
/// non-blocking, allocation-free and safe to call from the audio thread.
pub struct RtHostServices { pub fn log(&self, level: LogLevel, msg: &str);
    pub fn request_callback(&self); pub fn request_process(&self); pub fn request_restart(&self);
    pub fn schedule_worker(&self, task: TaskId) -> bool; }
```

Provide `HostServices::null()` / `RtHostServices::null()` no-op implementations so tests,
offline rendering and unhosted previews never need a real host.

---

## `daux-core`

The format-neutral object model. This is the crate the adapters translate to and from.

```rust
pub struct DauxError { .. }   pub enum ErrorKind { InvalidArgument, Unsupported, OutOfMemory,
    InvalidState, WrongThread, NotRealtimeSafe, AbiMismatch, VersionMismatch, NotFound,
    Io, Graphics, Host, Plugin, Internal }
impl DauxError { pub fn new(kind: ErrorKind, msg: impl Into<String>) -> Self;
                 pub fn kind(&self) -> ErrorKind; pub fn status_code(&self) -> i32; }
pub type DauxResult<T> = Result<T, DauxError>;

pub struct PluginId(String);   // validated: reverse-DNS, ASCII, ≤127 bytes
pub struct Version { major, minor, patch, build: u32 }
pub enum Category { Effect, Instrument, MidiEffect, Analyzer, Generator, Utility, Unknown }
pub struct Capabilities(u64);  // mirrors DAUX_CAP_*, builder-style `with_*` methods
pub struct PluginDescriptor { pub id: PluginId, pub name: String, pub vendor: String,
    pub version: Version, pub description: String, pub url: String, pub support_url: String,
    pub copyright: String, pub license: String, pub category: Category,
    pub capabilities: Capabilities, pub features: Vec<String>,
    pub sample_formats: SampleFormats, pub state_schema_version: u32,
    pub min_abi: (u32, u32) }
impl PluginDescriptor { pub fn builder(id: &str, name: &str) -> PluginDescriptorBuilder; }

pub enum ProcessMode { Realtime, Offline, Prefetch, Analysis }
pub struct ProcessConfig { pub sample_rate: f64, pub min_block_size: u32,
    pub max_block_size: u32, pub sample_format: SampleFormat, pub process_mode: ProcessMode }
pub enum ProcessStatus { Error, Continue, ContinueIfNotQuiet, Tail, Sleep }
pub enum Tail { None, Samples(u32), Infinite, Unknown }
pub enum Latency { Zero, Samples(u32) }

/// Everything a `process` call may touch, all borrowed for the call.
pub struct ProcessContext<'a> {
    pub fn frames(&self) -> usize;
    pub fn transport(&self) -> Option<&Transport>;
    pub fn steady_time(&self) -> Option<i64>;
    pub fn config(&self) -> &ProcessConfig;
    pub fn host(&self) -> &RtHostServices; }

/// The audio-thread half of a plug-in. `process` obeys abi-v1 §8 without exception.
pub trait DauxProcessor: Send {
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()>;   // [main-thread] allocates here
    fn activate(&mut self) -> DauxResult<()> { Ok(()) }                // [audio-thread]
    fn deactivate(&mut self) {}
    fn reset(&mut self) {}
    fn process<'a>(&mut self, ctx: &ProcessContext<'a>, audio: &mut AudioBuses<'a, f32>,
                   events: &mut ProcessEvents<'a>) -> ProcessStatus;
    fn process_f64<'a>(&mut self, ..) -> ProcessStatus { ProcessStatus::Error }  // opt-in
    fn latency(&self) -> Latency { Latency::Zero }
    fn tail(&self) -> Tail { Tail::None }
}

pub struct ProcessEvents<'a> { pub fn input(&self) -> &dyn InputEvents;
                               pub fn output(&mut self) -> &mut dyn OutputEvents; }

/// The main-thread half: parameters, state, host communication, editor creation.
pub trait DauxController: Send {
    fn params(&self) -> &dyn Params;
    fn save_state(&self, w: &mut StateWriter) -> DauxResult<()>;
    fn load_state(&mut self, r: &StateReader) -> DauxResult<()>;
    fn set_host(&mut self, _host: HostServices) {}
    fn on_main_thread(&mut self) {}
    fn on_worker(&mut self, _task: TaskId) {}
}

pub trait DauxPlugin: Send + 'static {
    fn descriptor() -> PluginDescriptor where Self: Sized;
    fn bus_layout(&self) -> BusLayout;
    fn event_ports(&self) -> EventPortLayout { EventPortLayout::default() }
    fn processor(&mut self) -> &mut dyn DauxProcessor;
    fn controller(&mut self) -> &mut dyn DauxController;
    /// `None` for headless plug-ins. The editor may be created and destroyed repeatedly
    /// and its lifetime is independent of the processor's.
    /// Not `Send`: an editor lives on the host's callback thread, and no real UI toolkit is
    /// `Send`. The audio thread cannot reach this method.
    fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> { None }
    fn accepts_bus_layout(&self, _layout: &BusLayout) -> bool { true }
}

pub trait DauxFactory: Send + Sync + 'static {
    fn plugin_count(&self) -> usize;
    fn descriptor(&self, index: usize) -> Option<PluginDescriptor>;
    fn create(&self, id: &str) -> DauxResult<Box<dyn DauxPlugin>>;
}
```

`create_editor` returns an opaque `Any` here because `daux-core` must not depend on
`daux-graphics`; `daux-plugin-api` re-types it into `Box<dyn DauxGraphic>`.

---

## `daux-graphics`

```rust
pub struct LogicalSize { pub width: f64, pub height: f64 }
pub struct PhysicalSize { pub width: u32, pub height: u32 }
pub enum GraphicFramework { Egui, Gpui, Custom }
pub enum GraphicRenderer { Wgpu, OpenGl, Software }
pub enum PresentationMode { NativeWindow, EmbeddedSurface, SharedTexture, ExternalWindow }
pub struct GraphicDescriptor { pub framework: GraphicFramework, pub renderer: GraphicRenderer,
    pub preferred_size: LogicalSize, pub min_size: Option<LogicalSize>,
    pub max_size: Option<LogicalSize>, pub resizable: bool, pub keeps_aspect: Option<f64>,
    pub preferred_presentation: PresentationMode }

pub enum WindowTarget { Win32 { hwnd: *mut c_void },
                        Cocoa { ns_view: *mut c_void },
                        X11 { window: u64, display: *mut c_void },
                        Wayland { surface: *mut c_void, display: *mut c_void } }
impl WindowTarget { pub fn from_raw_window_handle(..) -> Option<Self>; }

pub struct GraphicContext<'a> { pub fn target(&self) -> &WindowTarget;
    pub fn scale_factor(&self) -> f64; pub fn size(&self) -> PhysicalSize;
    pub fn host(&self) -> &HostServices; pub fn presentation(&self) -> PresentationMode; }

/// Neither `Send` nor `Sync`: main-thread only, by construction. GPUI and egui are both
/// `Rc`-based, so a `Send` bound here would rule out every real backend.
pub trait DauxGraphic {
    fn descriptor(&self) -> GraphicDescriptor;
    fn capabilities(&self) -> GraphicCapabilities { self.descriptor().capabilities }
    fn open(&mut self, ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()>;
    fn resize(&mut self, size: PhysicalSize) -> DauxGraphicResult<()>;
    fn scale_factor_changed(&mut self, scale: ScaleFactor) {}
    fn on_input(&mut self, _event: &InputEvent) -> InputResponse { InputResponse::Ignored }
    fn tick(&mut self) {}                 // host-driven frame/idle callback
    fn close(&mut self);
}
pub enum InputEvent { PointerMoved { x: f64, y: f64 }, PointerButton { .. }, Scroll { .. },
                      Key { .. }, Text { .. }, Focus(bool), Modifiers(Modifiers) }

pub struct SharedTextureCaps { pub kinds: Vec<SharedTextureKind>, pub formats: Vec<TextureFormat> }
pub enum SharedTextureKind { D3D11Shared, D3D12Heap, IoSurface, DmaBuf, VulkanFd, VulkanWin32 }
pub struct SharedTexture { pub kind: SharedTextureKind, pub handle: *mut c_void,
    pub format: TextureFormat, pub size: PhysicalSize, pub row_pitch: u32,
    pub fence: Option<*mut c_void> }
pub trait SharedTexturePresenter { fn negotiate(&mut self, host: &SharedTextureCaps)
                                       -> Option<SharedTextureKind>;
                                   fn acquire(&mut self) -> Option<SharedTexture>; }

/// Parameter ↔ widget glue that every backend reuses: gesture bookkeeping, text entry,
/// and value ↔ normalised conversion.
pub struct ParamBinding<'a> { pub fn new(param: &'a dyn Param, host: Option<&'a dyn HostParams>) -> Self;
    pub fn begin_gesture(&self); pub fn set_normalized(&self, v: f64); pub fn end_gesture(&self);
    pub fn display(&self) -> String; }
```

`daux-graphics` must not depend on any GUI framework or GPU API. Backends implement
`DauxGraphic` in their own crates.

---

## `daux-bundle`

```rust
pub struct TargetId(String);   // "windows-x86_64" | "linux-aarch64" | "macos-universal" …
impl TargetId { pub fn host() -> Self; pub fn from_rust_triple(t: &str) -> Option<Self>;
                pub fn to_rust_triples(&self) -> &'static [&'static str];
                pub fn dylib_extension(&self) -> &'static str; }

pub enum BundleLayout { Posix,  // Content/{target}/, Library/{target}/, Resources/, manifest.json
                        Apple } // Contents/{Info.plist,MacOS,Frameworks,Resources}

pub struct Manifest { pub format: String, pub format_version: u32, pub abi_version: u32,
    pub plugin: ManifestPlugin, pub targets: Vec<TargetId>, pub capabilities: ManifestCaps,
    pub graphics: Option<ManifestGraphics>, pub dependencies: Vec<String>,
    pub resources: Option<ManifestResources> }      // serde Serialize + Deserialize
pub struct ManifestPlugin { pub id, name, vendor, version, description: String }

/// Layout-independent view produced from manifest.json *or* Info.plist.
pub struct BundleMetadata { pub id: String, pub name: String, pub vendor: String,
    pub version: String, pub description: String, pub format_version: u32,
    pub abi_version: u32, pub targets: Vec<TargetId>, pub capabilities: ManifestCaps,
    pub graphics: Option<ManifestGraphics> }

pub struct Bundle { pub fn open(path: &Path) -> BundleResult<Self>;
    pub fn path(&self) -> &Path; pub fn layout(&self) -> BundleLayout;
    pub fn metadata(&self) -> &BundleMetadata;
    /// Path of the dynamic library for `target`, or `NoBinaryForTarget`.
    pub fn binary_path(&self, target: &TargetId) -> BundleResult<PathBuf>;
    /// Directory holding bundled dependencies for `target`, if any.
    pub fn library_dir(&self, target: &TargetId) -> Option<PathBuf>;
    pub fn resources(&self) -> ResourceDir;
    pub fn validate(&self) -> Vec<ValidationIssue>; }

/// Every lookup is confined to the bundle: `..`, absolute paths, drive letters,
/// Windows device names and symlink escapes are rejected with `PathEscape`.
pub struct ResourceDir { pub fn read(&self, logical: &str) -> BundleResult<Vec<u8>>;
    pub fn read_to_string(&self, logical: &str) -> BundleResult<String>;
    pub fn resolve(&self, logical: &str) -> BundleResult<PathBuf>;
    pub fn exists(&self, logical: &str) -> bool; }

pub struct BundleBuilder { pub fn new(id, name, vendor, version) -> Self;
    pub fn layout(self, l: BundleLayout) -> Self;
    pub fn binary(self, target: TargetId, from: &Path) -> Self;
    pub fn library(self, target: TargetId, from: &Path) -> Self;
    pub fn resource_dir(self, from: &Path) -> Self;
    pub fn capabilities(self, c: ManifestCaps) -> Self;
    pub fn write(self, out_dir: &Path) -> BundleResult<PathBuf>; }

pub struct ValidationIssue { pub severity: Severity, pub code: &'static str, pub message: String }
pub enum Severity { Error, Warning, Info }
pub struct BundleError { .. }   pub type BundleResult<T> = Result<T, BundleError>;
```

Hostile input is expected: bound every allocation from parsed metadata (reject manifests
over 4 MiB, strings over 4 KiB, more than 256 targets), never panic on malformed data.

---

## `daux-protocol` / `daux-ipc`

`daux-protocol`: `#[repr(C)]` control-plane messages (`CreateInstance`, `Activate`,
`LoadState`, `OpenEditor`, `ReportLatency`, `Error`, …) with a length-prefixed framing
codec, plus data-plane structures (`AudioBlockHeader`, `EventRecord`, `TransportSnapshot`)
sized for shared memory. Encoding is explicit little-endian, never `serde`, never JSON.

`daux-ipc`: `trait ControlTransport { fn send(&mut self, frame: &[u8]) -> IpcResult<()>;
fn recv(&mut self, buf: &mut Vec<u8>) -> IpcResult<usize>; }`, a
`trait DataPlane { fn audio_regions(&self) -> …; }`, an in-process `LoopbackTransport`
implementation for tests, and `SharedRegion` describing a mapped audio buffer. Platform
transports (named pipes, unix sockets, shared memory) are behind `cfg` and MAY be
unimplemented in v1 as long as the traits and the loopback path are real.

---

## `daux-plugin-api`

Re-exports and ergonomics only. It must **not** define a second, competing set of traits.

```rust
pub use daux_core::*;  pub use daux_audio::*;  /* events, midi, parameter, state,
                                                  transport, host_services, graphics, rt */
pub mod prelude { /* the curated set a plug-in author needs in scope */ }

/// Blanket glue turning a `DauxPlugin` implementor into something the adapters can drive
/// without knowing its concrete type.
pub struct PluginInstance { pub fn new(p: Box<dyn DauxPlugin>) -> Self; /* … */ }
/// Trivial single-plug-in factory: `SingleFactory::<MyPlugin>::new()`.
pub struct SingleFactory<P: DauxPlugin + Default> { .. }
/// Multi-plug-in factory built from registered constructors.
pub struct PluginRegistry { pub fn new() -> Self;
    pub fn register<P: DauxPlugin + Default>(&mut self) -> &mut Self; }
impl DauxFactory for PluginRegistry { .. }
```

## `daux-plugin-macros`

`#[derive(DauxParams)]` — generates `impl Params` from `#[param(id = .., name = ..,
range = a..=b, unit = "..", curve = "log", default = ..)]` fields, with compile-time
duplicate-id detection and a clear error span per field.

`#[derive(DauxPlugin)]` with `#[plugin(id = .., name = .., vendor = .., version = ..,
category = .., capabilities(..))]` — generates `descriptor()` only; it never generates DSP.

`#[derive(DauxState)]` — generates `save_state`/`load_state` over annotated fields.

Macros emit code that refers to `::daux_plugin::__private::*` re-exports so that a plug-in
crate only ever depends on `daux-plugin`.

## `daux-plugin`

The facade. `pub use daux_plugin_api::*;` plus:

```rust
pub mod prelude { pub use daux_plugin_api::prelude::*;
                  #[cfg(feature = "derive")] pub use daux_plugin_macros::*; }
/// Emits every enabled format entry point for a factory type.
#[macro_export] macro_rules! export_plugin { ($factory:ty) => { /* cfg-gated per format */ } }
pub mod __private { /* re-exports used by generated code */ }
```

---

## `daux-format-axt`, `-vst3`, `-clap`

Each exposes exactly one public entry macro plus the glue it needs:

```rust
daux_format_axt::export_entry!(MyFactory);     // → daux_plugin_entry_v1
daux_format_vst3::export_entry!(MyFactory);    // → GetPluginFactory / InitDll / ExitDll
daux_format_clap::export_entry!(MyFactory);    // → clap_entry
```

All three wrap every exported function in `catch_unwind`, convert panics to the format's
error code, and poison the instance (abi-v1 §17). None of them may leak format types into
`daux-core`. Capability mappings that cannot be expressed are reported through
`pub fn compatibility_report(d: &PluginDescriptor) -> Vec<CompatibilityWarning>` so
`daux build` can print them.

## `daux-runtime`

```rust
pub struct AxtModule { pub fn load(bundle: &Bundle, target: &TargetId) -> RuntimeResult<Self>;
    pub fn entry(&self) -> &DauxPluginEntryV1; pub fn abi_version(&self) -> (u32, u32); }
pub struct LoadedFactory { pub fn create(module: Arc<AxtModule>, host: HostBridge)
        -> RuntimeResult<Self>;
    pub fn plugin_count(&self) -> usize;
    pub fn descriptor(&self, i: usize) -> RuntimeResult<PluginDescriptor>;
    pub fn create_plugin(&self, id: &str) -> RuntimeResult<LoadedPlugin>; }
pub struct LoadedPlugin { pub fn activate(&mut self, config: &ProcessConfig) -> RuntimeResult<()>;
    pub fn start_processing / stop_processing / reset / deactivate;
    pub fn process(&mut self, block: &mut HostBlock<'_>) -> ProcessStatus;
    pub fn params(&self) -> Option<ParamsExt<'_>>; pub fn state(&self) -> Option<StateExt<'_>>;
    pub fn gui(&self) -> Option<GuiExt<'_>>; pub fn latency(&self) -> u32; }
pub struct HostBridge { /* builds DauxHostV1 from Rust host-service impls */ }
```

The module keeps the `libloading::Library` alive inside an `Arc` that every derived object
holds, so unloading before the last handle is dropped is impossible by construction.
Dependency directories are added with `AddDllDirectory`/`LOAD_LIBRARY_SEARCH_USER_DIRS` on
Windows and `$ORIGIN` rpath on Linux — never by mutating `PATH`/`LD_LIBRARY_PATH`.

## `daux-scan`, `daux-host`, `daux-cli`

```rust
// daux-scan
pub struct Scanner { pub fn new() -> Self; pub fn with_cache(path: PathBuf) -> Self;
    pub fn add_search_path(&mut self, p: PathBuf); pub fn default_search_paths() -> Vec<PathBuf>;
    pub fn scan(&mut self) -> ScanReport; pub fn scan_one(path: &Path) -> ScanResult<ScanEntry>; }
pub struct ScanEntry { pub path: PathBuf, pub format: PluginFormat, pub metadata: BundleMetadata,
    pub descriptors: Vec<PluginDescriptor>, pub scanned_at: SystemTime, pub fingerprint: u64 }
pub enum PluginFormat { Axt, Vst3, Clap }

// daux-host
pub struct TestHost { pub fn new(config: ProcessConfig) -> Self;
    pub fn load(&mut self, bundle: &Path) -> HostResult<InstanceId>;
    pub fn set_param(&mut self, i: InstanceId, id: u32, value: f64);
    pub fn send_note_on(&mut self, i: InstanceId, time, key, velocity);
    pub fn process(&mut self, i: InstanceId, input: &AudioStorage<f32>,
                   output: &mut AudioStorage<f32>) -> HostResult<ProcessStatus>;
    pub fn save_state(&mut self, i: InstanceId) -> HostResult<Vec<u8>>;
    pub fn load_state(&mut self, i: InstanceId, bytes: &[u8]) -> HostResult<()>; }

// daux-cli — clap derive; subcommands: new, build, bundle, validate, inspect, scan, test, run
```

`daux build` reads one source of truth: `[package.metadata.daux]` in the plug-in's
`Cargo.toml` (see `docs/specifications/manifest-v1.md` §2). It generates `manifest.json`
and `Info.plist`; the developer never writes them by hand.
