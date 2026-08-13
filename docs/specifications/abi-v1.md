# DAUx Native ABI — Specification v1

Status: **Stable draft** · ABI version: `1` · Entry symbol: `daux_plugin_entry_v1`

This document is the binary contract between a DAUx host and a DAUx Audio Extension
(`.axt`). It is normative. `crates/daux-abi` is the reference transcription of this
document into Rust; where the two disagree, **this document wins** and `daux-abi` is a bug.

Key words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, **MAY** are used as in RFC 2119.

---

## 1. Scope and design rules

The ABI is crossed by two independently compiled dynamic modules. Neither side may assume
the other was built by the same compiler, the same Rust version, the same allocator, the
same standard library, or even the same language.

Therefore, across this boundary:

| Forbidden                                   | Required instead                              |
| ------------------------------------------- | --------------------------------------------- |
| Rust `enum`                                 | `u32`/`i32` with documented constants          |
| Rust `String`, `Vec`, `Box`, `&T`, `&mut T` | `DauxStrView`, raw pointers + explicit length  |
| Rust trait objects, generics                | opaque handle + `#[repr(C)]` function table    |
| `Result`, `Option<T>` (non-pointer)         | `DauxStatus` return codes                      |
| Panic / unwinding                           | `DauxStatus` error codes                       |
| Cross-module `free`                         | caller-owned buffers or owner-provided destroy |
| `bool`, `usize` in packed layouts           | `DauxBool` (`u32`), fixed-width integers       |

Every `#[repr(C)]` structure in this specification is **append-only**. A future minor
revision MAY add fields at the tail; it MUST NOT reorder, resize, repurpose or remove an
existing field. Every structure that can grow carries `size: u32` as its first field.

---

## 2. Primitive types

```rust
/// Result of an ABI call. `0` is success; negative values are errors.
#[repr(transparent)]
pub struct DauxStatus(pub i32);

pub const DAUX_OK:                 DauxStatus = DauxStatus(0);
pub const DAUX_ERR_UNKNOWN:        DauxStatus = DauxStatus(-1);
pub const DAUX_ERR_INVALID_ARG:    DauxStatus = DauxStatus(-2);
pub const DAUX_ERR_UNSUPPORTED:    DauxStatus = DauxStatus(-3);
pub const DAUX_ERR_OUT_OF_MEMORY:  DauxStatus = DauxStatus(-4);
pub const DAUX_ERR_INVALID_STATE:  DauxStatus = DauxStatus(-5);
pub const DAUX_ERR_WRONG_THREAD:   DauxStatus = DauxStatus(-6);
pub const DAUX_ERR_NOT_REALTIME:   DauxStatus = DauxStatus(-7);
pub const DAUX_ERR_ABI_MISMATCH:   DauxStatus = DauxStatus(-8);
pub const DAUX_ERR_VERSION:        DauxStatus = DauxStatus(-9);
pub const DAUX_ERR_NOT_FOUND:      DauxStatus = DauxStatus(-10);
pub const DAUX_ERR_IO:             DauxStatus = DauxStatus(-11);
pub const DAUX_ERR_GRAPHICS:       DauxStatus = DauxStatus(-12);
pub const DAUX_ERR_HOST:           DauxStatus = DauxStatus(-13);
pub const DAUX_ERR_PLUGIN:         DauxStatus = DauxStatus(-14);
pub const DAUX_ERR_PANIC:          DauxStatus = DauxStatus(-15);
pub const DAUX_ERR_INTERNAL:       DauxStatus = DauxStatus(-16);
```

```rust
/// C-compatible boolean. Producers MUST write exactly 0 or 1.
/// Consumers MUST treat any non-zero value as true.
pub type DauxBool = u32;
pub const DAUX_FALSE: DauxBool = 0;
pub const DAUX_TRUE:  DauxBool = 1;

/// Borrowed UTF-8 text. NOT NUL-terminated; `len` is in bytes.
/// A `DauxStrView` passed as an argument is valid only for the duration of the call.
/// `ptr` MAY be null iff `len == 0`.
#[repr(C)]
pub struct DauxStrView { pub ptr: *const u8, pub len: usize }

/// Four-component version. Ordering is lexicographic over (major, minor, patch, build).
#[repr(C)]
pub struct DauxVersion { pub major: u32, pub minor: u32, pub patch: u32, pub build: u32 }
```

### 2.1 Fixed text buffers

Any string **written by the callee into caller memory** uses a fixed-size UTF-8 buffer,
NUL-padded, not necessarily NUL-terminated when full. This removes every cross-module
allocation and lifetime question from metadata paths.

```rust
pub const DAUX_NAME_SIZE: usize = 64;
pub const DAUX_TEXT_SIZE: usize = 256;
pub const DAUX_PATH_SIZE: usize = 1024;
pub const DAUX_ID_SIZE:   usize = 128;

#[repr(C)] pub struct DauxName([u8; DAUX_NAME_SIZE]);
#[repr(C)] pub struct DauxText([u8; DAUX_TEXT_SIZE]);
#[repr(C)] pub struct DauxPath([u8; DAUX_PATH_SIZE]);
#[repr(C)] pub struct DauxId  ([u8; DAUX_ID_SIZE]);
```

Writers MUST truncate on a UTF-8 character boundary. Readers MUST tolerate invalid UTF-8
by lossy conversion and MUST NOT panic. Trailing NUL bytes are not part of the value.

### 2.2 Opaque handles

```rust
#[repr(transparent)] pub struct DauxFactoryHandle(pub *mut c_void);
#[repr(transparent)] pub struct DauxPluginHandle(pub *mut c_void);
#[repr(transparent)] pub struct DauxHostHandle(pub *mut c_void);
```

A handle is meaningful only to the module that produced it. The receiving module MUST
treat it as an opaque token, MUST NOT dereference it, and MUST NOT let it outlive the
object it names (§16).

### 2.3 Interface pairs

An interface is a handle plus a pointer to a function table owned by the producing module.

```rust
#[repr(C)] pub struct DauxFactoryV1 { pub handle: DauxFactoryHandle, pub api: *const DauxFactoryApiV1 }
#[repr(C)] pub struct DauxPluginV1  { pub handle: DauxPluginHandle,  pub api: *const DauxPluginApiV1 }
#[repr(C)] pub struct DauxHostV1    { pub handle: DauxHostHandle,    pub api: *const DauxHostApiV1 }
```

Function tables are immutable and MUST remain valid for as long as the producing module is
loaded. Optional entries are `Option<unsafe extern "C" fn(..)>`; a null pointer means
"not supported" and callers MUST check before calling.

---

## 3. Version negotiation

```rust
pub const DAUX_ABI_VERSION_MAJOR: u32 = 1;
pub const DAUX_ABI_VERSION_MINOR: u32 = 0;
pub const DAUX_ABI_MAGIC: u64 = 0x4441_5558_4142_4931; // "DAUXABI1" big-endian
```

* Major version identifies the entry symbol and the shape of this document.
  `daux_plugin_entry_v1` **always** implies `abi_version_major == 1`.
* Minor version identifies tail extensions of v1 structures. A host MUST accept a plug-in
  with a lower or higher minor version.
* A reader MUST validate `size` before touching any field beyond the ones it knows:
  a field at offset `O` of width `W` is present iff `size >= O + W`.
* A reader MUST ignore unknown tail bytes.
* A writer MUST zero every field it does not populate, including `reserved` arrays.

Rejection rules — the host MUST refuse to load when any of these hold:

1. `daux_plugin_entry_v1` is missing or returns null.
2. `magic != DAUX_ABI_MAGIC`.
3. `abi_version_major != 1`.
4. `size` is smaller than the minimum v1.0 size of the structure.

---

## 4. Entry point

Every `.axt` binary MUST export exactly one symbol per supported ABI generation:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn daux_plugin_entry_v1() -> *const DauxPluginEntryV1;
```

The returned pointer MUST be non-null, MUST point at storage with `'static` lifetime, and
MUST be identical across calls. The function MUST be callable before any other DAUx symbol,
MUST NOT block, MUST NOT allocate unbounded memory, and MUST NOT touch the filesystem,
the network, GPU devices, or any GUI subsystem. It is called on the **main thread**.

```rust
#[repr(C)]
pub struct DauxPluginEntryV1 {
    pub size: u32,
    pub abi_version_major: u32,
    pub abi_version_minor: u32,
    pub _pad0: u32,
    pub magic: u64,

    /// Identifies the SDK that produced the binary. Diagnostics only.
    pub sdk_name: DauxName,
    pub sdk_version: DauxVersion,

    /// Called once after the host has validated the header. `host` MUST remain valid
    /// until `destroy_factory` returns. [main-thread]
    pub create_factory: unsafe extern "C" fn(
        host: *const DauxHostV1,
        out_factory: *mut DauxFactoryV1,
    ) -> DauxStatus,

    /// Releases the factory. All plug-in instances created from it MUST already be
    /// destroyed. [main-thread]
    pub destroy_factory: unsafe extern "C" fn(factory: DauxFactoryV1),

    pub reserved: [usize; 8],
}
```

Loading sequence (normative order):

```
open library → resolve daux_plugin_entry_v1 → validate magic/version/size
  → create_factory → enumerate descriptors → create_plugin → init → activate
```

`dlopen`/`LoadLibrary` MUST NOT be assumed to run plug-in code beyond static
initialisers; all real work happens in `create_factory` or later.

---

## 5. Factory

```rust
#[repr(C)]
pub struct DauxFactoryApiV1 {
    pub size: u32,
    pub _pad0: u32,

    /// Number of plug-ins in this binary. [any-thread]
    pub plugin_count: unsafe extern "C" fn(f: DauxFactoryHandle) -> u32,

    /// Fills `out` with the descriptor at `index`. Lightweight: it MUST NOT
    /// instantiate DSP, load resources, or touch the GPU. [any-thread]
    pub descriptor: unsafe extern "C" fn(
        f: DauxFactoryHandle, index: u32, out: *mut DauxPluginDescriptorV1,
    ) -> DauxStatus,

    /// Instantiates the plug-in with the given stable id. [main-thread]
    pub create_plugin: unsafe extern "C" fn(
        f: DauxFactoryHandle, id: DauxStrView, out: *mut DauxPluginV1,
    ) -> DauxStatus,

    /// Factory-level extension lookup; null when unsupported. [any-thread]
    pub get_extension: Option<
        unsafe extern "C" fn(f: DauxFactoryHandle, id: DauxStrView) -> *const c_void,
    >,

    pub reserved: [usize; 6],
}
```

A plug-in instance is destroyed through `DauxPluginApiV1::destroy`, not through the
factory. The factory MUST outlive every instance it created.

---

## 6. Plug-in descriptor

```rust
#[repr(C)]
pub struct DauxPluginDescriptorV1 {
    pub size: u32,
    pub min_abi_version_major: u32,
    pub min_abi_version_minor: u32,
    pub _pad0: u32,

    pub id: DauxId,             // stable, reverse-DNS, e.g. "studio.futureboard.equzx"
    pub name: DauxName,
    pub vendor: DauxName,
    pub version: DauxVersion,
    pub version_string: DauxName,
    pub description: DauxText,
    pub url: DauxText,
    pub support_url: DauxText,
    pub copyright: DauxText,
    pub license: DauxName,

    /// `DAUX_CATEGORY_*`.
    pub category: u32,
    /// Bitset of `DAUX_SAMPLE_FORMAT_*` the processor can accept.
    pub sample_formats: u32,
    /// Bitset of `DAUX_CAP_*`.
    pub capabilities: u64,
    /// Schema version of the plug-in's persisted state (§12).
    pub state_schema_version: u32,
    /// Semicolon-separated free-form tags, e.g. "eq;dynamics;mastering".
    pub _pad1: u32,
    pub features: DauxText,

    pub reserved: [usize; 8],
}
```

### 6.1 Categories

```
DAUX_CATEGORY_UNKNOWN      0
DAUX_CATEGORY_EFFECT       1
DAUX_CATEGORY_INSTRUMENT   2
DAUX_CATEGORY_MIDI_EFFECT  3
DAUX_CATEGORY_ANALYZER     4
DAUX_CATEGORY_GENERATOR    5
DAUX_CATEGORY_UTILITY      6
```

### 6.2 Capability bits

```
DAUX_CAP_AUDIO_EFFECT          1 << 0
DAUX_CAP_INSTRUMENT            1 << 1
DAUX_CAP_MIDI_EFFECT           1 << 2
DAUX_CAP_ANALYZER              1 << 3
DAUX_CAP_MIDI_INPUT            1 << 4
DAUX_CAP_MIDI_OUTPUT           1 << 5
DAUX_CAP_MIDI2                 1 << 6
DAUX_CAP_SIDECHAIN             1 << 7
DAUX_CAP_DYNAMIC_BUSES         1 << 8
DAUX_CAP_SAMPLE_ACCURATE_AUTO  1 << 9
DAUX_CAP_NOTE_EXPRESSION       1 << 10
DAUX_CAP_HAS_GUI               1 << 11
DAUX_CAP_REQUIRES_GUI          1 << 12
DAUX_CAP_SHARED_TEXTURE_GUI    1 << 13
DAUX_CAP_OFFLINE_RENDER        1 << 14
DAUX_CAP_HARD_REALTIME         1 << 15
DAUX_CAP_SANDBOX_SAFE          1 << 16
DAUX_CAP_STEREO_ONLY           1 << 17
DAUX_CAP_LATENCY_DYNAMIC       1 << 18
DAUX_CAP_TAIL_INFINITE         1 << 19
```

### 6.3 Sample formats

```
DAUX_SAMPLE_FORMAT_F32  1 << 0
DAUX_SAMPLE_FORMAT_F64  1 << 1
```

---

## 7. Plug-in instance

```rust
#[repr(C)]
pub struct DauxPluginApiV1 {
    pub size: u32,
    pub _pad0: u32,

    /// Late initialisation. The instance is created but not yet usable until this
    /// returns `DAUX_OK`. Extensions MAY be queried after this point. [main-thread]
    pub init: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,

    /// Destroys the instance. MUST be preceded by `deactivate` if activated. [main-thread]
    pub destroy: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Allocates DSP resources for the given configuration. [main-thread]
    pub activate: unsafe extern "C" fn(
        p: DauxPluginHandle, config: *const DauxProcessConfigV1,
    ) -> DauxStatus,

    /// Releases DSP resources. [main-thread]
    pub deactivate: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Called on the audio thread before the first `process` of a run. [audio-thread]
    pub start_processing: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,
    /// [audio-thread]
    pub stop_processing: unsafe extern "C" fn(p: DauxPluginHandle),

    /// Clears all internal audio state (delay lines, filters, voices).
    /// [audio-thread, only while not processing]
    pub reset: unsafe extern "C" fn(p: DauxPluginHandle),

    /// The real-time entry point. See §8. [audio-thread]
    pub process: unsafe extern "C" fn(
        p: DauxPluginHandle, process: *const DauxProcessV1,
    ) -> i32,

    /// Extension lookup. Only valid after `init`. [any-thread]
    pub get_extension: unsafe extern "C" fn(
        p: DauxPluginHandle, id: DauxStrView,
    ) -> *const c_void,

    /// Drains work queued for the main thread after `request_callback`. [main-thread]
    pub on_main_thread: unsafe extern "C" fn(p: DauxPluginHandle),

    pub reserved: [usize; 6],
}
```

Lifecycle state machine — any other transition is a host error and the plug-in MUST
return `DAUX_ERR_INVALID_STATE` rather than misbehave:

```
created ──init──> inactive ──activate──> active ──start_processing──> processing
                     ^                      |                              |
                     └──── deactivate ──────┘<──── stop_processing ────────┘
inactive ──destroy──> gone
```

---

## 8. Processing

```rust
pub const DAUX_PROCESS_MODE_REALTIME: u32 = 0;
pub const DAUX_PROCESS_MODE_OFFLINE:  u32 = 1;
pub const DAUX_PROCESS_MODE_PREFETCH: u32 = 2;
pub const DAUX_PROCESS_MODE_ANALYSIS: u32 = 3;

#[repr(C)]
pub struct DauxProcessConfigV1 {
    pub size: u32,
    pub sample_format: u32,   // exactly one DAUX_SAMPLE_FORMAT_* bit
    pub process_mode: u32,    // DAUX_PROCESS_MODE_*
    pub min_block_size: u32,
    pub max_block_size: u32,
    pub _pad0: u32,
    pub sample_rate: f64,
    pub reserved: [usize; 6],
}
```

`max_block_size` is an upper bound, **not** a promise. Every `process` call MAY pass any
`frame_count` in `1 ..= max_block_size`. Plug-ins MUST NOT assume a constant block size.

```rust
/// One audio bus for one block. Exactly one of `data32`/`data64` is non-null and MUST
/// match `DauxProcessConfigV1::sample_format`.
#[repr(C)]
pub struct DauxAudioBufferV1 {
    pub channel_count: u32,
    pub _pad0: u32,
    /// Array of `channel_count` pointers, each to `frame_count` samples.
    pub data32: *const *mut f32,
    pub data64: *const *mut f64,
    /// Bit `c` set ⇒ channel `c` is constant for the whole block (usually silence).
    /// Purely an optimisation hint; readers MUST tolerate a zero mask.
    pub constant_mask: u64,
}

#[repr(C)]
pub struct DauxProcessV1 {
    pub size: u32,
    pub frame_count: u32,

    /// Monotonic sample counter since processing started, or -1 if unavailable.
    pub steady_time: i64,

    /// Null when the host exposes no transport.
    pub transport: *const DauxTransportV1,

    pub audio_input_count: u32,
    pub audio_output_count: u32,
    pub audio_inputs: *const DauxAudioBufferV1,
    pub audio_outputs: *mut DauxAudioBufferV1,

    /// Never null. Empty lists are represented by `size() == 0`.
    pub in_events: *const DauxEventListV1,
    pub out_events: *const DauxEventListV1,

    pub reserved: [usize; 6],
}
```

Return values of `process`:

```
DAUX_PROCESS_ERROR             0   // outputs are undefined; host SHOULD silence them
DAUX_PROCESS_CONTINUE          1   // keep calling
DAUX_PROCESS_CONTINUE_IF_LOUD  2   // keep calling while output is non-silent
DAUX_PROCESS_TAIL              3   // input finished, tail still ringing out
DAUX_PROCESS_SLEEP             4   // output is silent and will remain so
```

Buffers MAY alias between input and output (in-place processing). A plug-in that cannot
process in place MUST copy internally. Input buffers MUST be treated as read-only; the
`*mut` type exists only so hosts can hand out one allocation for both directions.

Real-time obligations of `process` — see `docs/architecture/realtime.md` for enforcement:
no allocation, no free, no lock that can be held by a non-real-time thread, no file or
network I/O, no `dlopen`, no thread creation, no GUI call, no unbounded loop, no sleeping,
no waiting on another thread, no panic.

---

## 9. Events

Events are flat `#[repr(C)]` records with a common header, accessed through a
host-provided list interface. The list owns the storage; the plug-in MUST NOT retain
pointers past the end of `process`.

```rust
#[repr(C)]
pub struct DauxEventHeaderV1 {
    /// Total byte size of this event including the header.
    pub size: u32,
    /// Sample offset within the current block: `0 ..= frame_count - 1`.
    pub time: u32,
    /// `DAUX_EVENT_*`.
    pub kind: u16,
    /// `DAUX_EVENT_FLAG_*`.
    pub flags: u16,
    /// Which event port the event belongs to.
    pub port_index: u16,
    pub _pad0: u16,
}

pub const DAUX_EVENT_NOTE_ON:          u16 = 1;
pub const DAUX_EVENT_NOTE_OFF:         u16 = 2;
pub const DAUX_EVENT_NOTE_CHOKE:       u16 = 3;
pub const DAUX_EVENT_NOTE_END:         u16 = 4;   // plug-in → host
pub const DAUX_EVENT_NOTE_EXPRESSION:  u16 = 5;
pub const DAUX_EVENT_PARAM_VALUE:      u16 = 6;
pub const DAUX_EVENT_PARAM_MOD:        u16 = 7;
pub const DAUX_EVENT_PARAM_GESTURE_BEGIN: u16 = 8;
pub const DAUX_EVENT_PARAM_GESTURE_END:   u16 = 9;
pub const DAUX_EVENT_TRANSPORT:        u16 = 10;
pub const DAUX_EVENT_MIDI1:            u16 = 11;
pub const DAUX_EVENT_MIDI2:            u16 = 12;
pub const DAUX_EVENT_SYSEX:            u16 = 13;
pub const DAUX_EVENT_CUSTOM:           u16 = 0x7000; // vendor range starts here

pub const DAUX_EVENT_FLAG_IS_LIVE:      u16 = 1 << 0; // performed live, not automation
pub const DAUX_EVENT_FLAG_DONT_RECORD:  u16 = 1 << 1;
```

```rust
#[repr(C)]
pub struct DauxEventNoteV1 {
    pub header: DauxEventHeaderV1,
    /// Host-assigned voice id, or -1 when the host does not track voices.
    pub note_id: i32,
    pub channel: i16,
    pub key: i16,      // 0..=127, or -1 as a wildcard on note-off/choke
    pub _pad0: i32,
    pub velocity: f64, // 0.0 ..= 1.0
    pub tuning: f64,   // cents offset from equal temperament
}

#[repr(C)]
pub struct DauxEventNoteExpressionV1 {
    pub header: DauxEventHeaderV1,
    pub expression_id: u32, // DAUX_NOTE_EXPR_*
    pub note_id: i32,
    pub channel: i16,
    pub key: i16,
    pub _pad0: u32,
    pub value: f64,
}

pub const DAUX_NOTE_EXPR_VOLUME:     u32 = 0;
pub const DAUX_NOTE_EXPR_PAN:        u32 = 1;
pub const DAUX_NOTE_EXPR_TUNING:     u32 = 2;
pub const DAUX_NOTE_EXPR_VIBRATO:    u32 = 3;
pub const DAUX_NOTE_EXPR_EXPRESSION: u32 = 4;
pub const DAUX_NOTE_EXPR_BRIGHTNESS: u32 = 5;
pub const DAUX_NOTE_EXPR_PRESSURE:   u32 = 6;

#[repr(C)]
pub struct DauxEventParamV1 {
    pub header: DauxEventHeaderV1,
    pub param_id: u32,
    /// -1 unless the change is scoped to a single voice.
    pub note_id: i32,
    pub channel: i16,
    pub key: i16,
    pub _pad0: u32,
    /// Absolute plain value for `PARAM_VALUE`; signed offset for `PARAM_MOD`.
    pub value: f64,
    /// Opaque host cookie, echoed back on output events. May be null.
    pub cookie: *mut c_void,
}

#[repr(C)]
pub struct DauxEventMidi1V1 {
    pub header: DauxEventHeaderV1,
    pub data: [u8; 3],
    pub _pad0: u8,
}

/// One MIDI 2.0 Universal MIDI Packet, 1–4 words, `word_count` valid words.
#[repr(C)]
pub struct DauxEventMidi2V1 {
    pub header: DauxEventHeaderV1,
    pub word_count: u32,
    pub words: [u32; 4],
}

/// SysEx bytes are borrowed from the event list and valid only during `process`.
#[repr(C)]
pub struct DauxEventSysExV1 {
    pub header: DauxEventHeaderV1,
    pub byte_count: u32,
    pub _pad0: u32,
    pub bytes: *const u8,
}
```

```rust
#[repr(C)]
pub struct DauxEventListV1 {
    pub size: u32,
    pub _pad0: u32,
    pub ctx: *mut c_void,

    /// Number of events. [audio-thread]
    pub count: unsafe extern "C" fn(ctx: *mut c_void) -> u32,

    /// Borrowed event at `index`, or null. The pointed-to record is valid until the
    /// current `process` returns. [audio-thread]
    pub get: unsafe extern "C" fn(ctx: *mut c_void, index: u32) -> *const DauxEventHeaderV1,

    /// Appends a copy of `event`. Returns `DAUX_ERR_OUT_OF_MEMORY` when the bounded
    /// output queue is full — this is a normal, non-fatal condition and the caller
    /// MUST NOT allocate to work around it. [audio-thread]
    pub push: unsafe extern "C" fn(
        ctx: *mut c_void, event: *const DauxEventHeaderV1,
    ) -> DauxStatus,

    pub reserved: [usize; 4],
}
```

Input events MUST be delivered sorted by `time`, then by list order for equal timestamps.
Output events SHOULD be pushed in non-decreasing `time` order; hosts MUST sort defensively.

---

## 10. Transport

```rust
pub const DAUX_TRANSPORT_HAS_TEMPO:      u32 = 1 << 0;
pub const DAUX_TRANSPORT_HAS_BEATS:      u32 = 1 << 1;
pub const DAUX_TRANSPORT_HAS_SECONDS:    u32 = 1 << 2;
pub const DAUX_TRANSPORT_HAS_TIME_SIG:   u32 = 1 << 3;
pub const DAUX_TRANSPORT_HAS_LOOP:       u32 = 1 << 4;
pub const DAUX_TRANSPORT_HAS_BAR:        u32 = 1 << 5;
pub const DAUX_TRANSPORT_IS_PLAYING:     u32 = 1 << 6;
pub const DAUX_TRANSPORT_IS_RECORDING:   u32 = 1 << 7;
pub const DAUX_TRANSPORT_IS_LOOPING:     u32 = 1 << 8;
pub const DAUX_TRANSPORT_IS_PREROLL:     u32 = 1 << 9;

#[repr(C)]
pub struct DauxTransportV1 {
    pub size: u32,
    pub flags: u32,

    pub song_pos_samples: i64,
    pub song_pos_beats: f64,
    pub song_pos_seconds: f64,

    pub tempo: f64,            // BPM
    pub tempo_increment: f64,  // BPM per sample, 0.0 when steady

    pub bar_start_beats: f64,
    pub bar_number: i32,
    pub time_sig_numerator: u16,
    pub time_sig_denominator: u16,

    pub loop_start_beats: f64,
    pub loop_end_beats: f64,
    pub loop_start_seconds: f64,
    pub loop_end_seconds: f64,

    pub reserved: [usize; 6],
}
```

A field is meaningful only when its `HAS_*` flag is set. Hosts MUST NOT fabricate values;
plug-ins MUST NOT read unflagged fields. A `DAUX_EVENT_TRANSPORT` event carries a
`DauxTransportV1` immediately after its header and signals a discontinuity (locate, loop
wrap, tempo jump) at a sample-accurate offset.

---

## 11. Standard extensions

Extensions are looked up by NUL-free UTF-8 id and return a pointer to a `#[repr(C)]`
function table owned by the providing module. Ids embed their version; a new version is a
new id. Unknown ids MUST return null rather than fail.

| Id                                             | Provider | Purpose                       |
| ---------------------------------------------- | -------- | ----------------------------- |
| `daux.audio-ports/1`                           | plug-in  | Bus topology                  |
| `daux.note-ports/1`                            | plug-in  | Event port topology           |
| `daux.params/1`                                | plug-in  | Parameter model               |
| `daux.state/1`                                 | plug-in  | Save / load                   |
| `daux.gui/1`                                   | plug-in  | Editor lifecycle              |
| `daux.latency/1`                               | plug-in  | Latency reporting             |
| `daux.tail/1`                                  | plug-in  | Tail length                   |
| `daux.render/1`                                | plug-in  | Realtime / offline switch     |
| `daux.host.log/1`                              | host     | Structured logging            |
| `daux.host.params/1`                           | host     | Automation gestures, rescan   |
| `daux.host.latency/1`                          | host     | Latency change notification   |
| `daux.host.tail/1`                             | host     | Tail change notification      |
| `daux.host.worker/1`                           | host     | Off-thread work scheduling    |
| `daux.host.gui/1`                              | host     | Resize requests, close        |
| `daux.host.timer/1`                            | host     | Periodic main-thread callback |
| `com.futureboard.daux.shared-texture/1`        | both     | GPU surface hand-off (§13)    |

Vendor extensions MUST use a reverse-DNS prefix. The `daux.` prefix is reserved.

### 11.1 `daux.audio-ports/1`

```rust
#[repr(C)]
pub struct DauxAudioPortInfoV1 {
    pub size: u32,
    pub id: u32,                 // stable across versions
    pub name: DauxName,
    pub channel_count: u32,
    pub layout: u32,             // DAUX_LAYOUT_*
    pub purpose: u32,            // DAUX_PORT_PURPOSE_*
    pub flags: u32,              // DAUX_PORT_FLAG_*
    pub reserved: [usize; 4],
}

pub const DAUX_PORT_FLAG_IS_MAIN:   u32 = 1 << 0;
pub const DAUX_PORT_FLAG_OPTIONAL:  u32 = 1 << 1; // may be deactivated by the host
pub const DAUX_PORT_FLAG_CV:        u32 = 1 << 2;
pub const DAUX_PORT_FLAG_SUPPORTS_64: u32 = 1 << 3;

#[repr(C)]
pub struct DauxAudioPortsApiV1 {
    pub size: u32,
    pub _pad0: u32,
    pub count: unsafe extern "C" fn(p: DauxPluginHandle, is_input: DauxBool) -> u32,
    pub get: unsafe extern "C" fn(
        p: DauxPluginHandle, index: u32, is_input: DauxBool,
        out: *mut DauxAudioPortInfoV1,
    ) -> DauxStatus,
    pub set_active: Option<unsafe extern "C" fn(
        p: DauxPluginHandle, index: u32, is_input: DauxBool, active: DauxBool,
    ) -> DauxStatus>,
    pub reserved: [usize; 4],
}
```

Channel layouts (`DAUX_LAYOUT_*`): `UNKNOWN 0`, `MONO 1`, `STEREO 2`, `L_R_C 3`,
`QUAD 4`, `SURROUND_2_1 5`, `SURROUND_5_1 6`, `SURROUND_7_1 7`, `ATMOS_7_1_4 8`,
`AMBISONIC_1ST 9`, `AMBISONIC_2ND 10`, `AMBISONIC_3RD 11`, `DISCRETE 12`, `CUSTOM 13`.

Port purposes (`DAUX_PORT_PURPOSE_*`): `MAIN 0`, `AUX 1`, `SIDECHAIN 2`, `MONITOR 3`,
`ANALYSIS 4`, `REFERENCE 5`, `CV 6`, `CONTROL 7`.

### 11.2 `daux.params/1`

```rust
pub const DAUX_PARAM_FLAG_AUTOMATABLE:      u32 = 1 << 0;
pub const DAUX_PARAM_FLAG_MODULATABLE:      u32 = 1 << 1;
pub const DAUX_PARAM_FLAG_PER_NOTE:         u32 = 1 << 2;
pub const DAUX_PARAM_FLAG_STEPPED:          u32 = 1 << 3;
pub const DAUX_PARAM_FLAG_READ_ONLY:        u32 = 1 << 4;
pub const DAUX_PARAM_FLAG_HIDDEN:           u32 = 1 << 5;
pub const DAUX_PARAM_FLAG_BYPASS:           u32 = 1 << 6;
pub const DAUX_PARAM_FLAG_REQUIRES_PROCESS: u32 = 1 << 7;
pub const DAUX_PARAM_FLAG_IS_METER:         u32 = 1 << 8;

#[repr(C)]
pub struct DauxParamInfoV1 {
    pub size: u32,
    pub id: u32,               // stable forever; see §14
    pub flags: u32,
    pub step_count: u32,       // 0 for continuous
    pub name: DauxName,
    pub group: DauxName,       // "" for top level, "/" separated otherwise
    pub unit: DauxName,        // "dB", "Hz", "%", ""
    pub min_value: f64,
    pub max_value: f64,
    pub default_value: f64,
    pub cookie: *mut c_void,   // plug-in private accelerator
    pub reserved: [usize; 4],
}

#[repr(C)]
pub struct DauxParamsApiV1 {
    pub size: u32,
    pub _pad0: u32,
    /// [main-thread]
    pub count: unsafe extern "C" fn(p: DauxPluginHandle) -> u32,
    /// [main-thread]
    pub get_info: unsafe extern "C" fn(
        p: DauxPluginHandle, index: u32, out: *mut DauxParamInfoV1,
    ) -> DauxStatus,
    /// [main-thread]
    pub get_value: unsafe extern "C" fn(
        p: DauxPluginHandle, id: u32, out: *mut f64,
    ) -> DauxStatus,
    /// Formats `value` into `out` (capacity `DAUX_TEXT_SIZE`). [main-thread]
    pub value_to_text: unsafe extern "C" fn(
        p: DauxPluginHandle, id: u32, value: f64, out: *mut DauxText,
    ) -> DauxStatus,
    /// [main-thread]
    pub text_to_value: unsafe extern "C" fn(
        p: DauxPluginHandle, id: u32, text: DauxStrView, out: *mut f64,
    ) -> DauxStatus,
    /// Applies parameter events while the plug-in is not processing.
    /// [main-thread when inactive, audio-thread otherwise]
    pub flush: unsafe extern "C" fn(
        p: DauxPluginHandle,
        in_events: *const DauxEventListV1,
        out_events: *const DauxEventListV1,
    ),
    pub reserved: [usize; 4],
}
```

Values crossing the ABI are always **plain** (real-world) values, never normalised.
Normalisation is a plug-in-side concern so that curve changes never break automation.

### 11.3 `daux.state/1`

```rust
/// A byte stream owned by the caller. Returns bytes transferred, or a negative
/// `DauxStatus` code on failure. A short read means end of stream.
#[repr(C)]
pub struct DauxStreamV1 {
    pub size: u32,
    pub _pad0: u32,
    pub ctx: *mut c_void,
    pub read: Option<unsafe extern "C" fn(ctx: *mut c_void, buf: *mut u8, len: usize) -> isize>,
    pub write: Option<unsafe extern "C" fn(ctx: *mut c_void, buf: *const u8, len: usize) -> isize>,
    pub reserved: [usize; 4],
}

#[repr(C)]
pub struct DauxStateApiV1 {
    pub size: u32,
    pub _pad0: u32,
    /// [main-thread]
    pub save: unsafe extern "C" fn(p: DauxPluginHandle, s: *const DauxStreamV1) -> DauxStatus,
    /// [main-thread]
    pub load: unsafe extern "C" fn(p: DauxPluginHandle, s: *const DauxStreamV1) -> DauxStatus,
    pub reserved: [usize; 4],
}
```

The host owns the stream and therefore the allocation — no memory crosses the module
boundary in either direction (§16.2).

### 11.4 `daux.gui/1`

```rust
pub const DAUX_WINDOW_API_WIN32:   u32 = 1;
pub const DAUX_WINDOW_API_COCOA:   u32 = 2;
pub const DAUX_WINDOW_API_X11:     u32 = 3;
pub const DAUX_WINDOW_API_WAYLAND: u32 = 4;

#[repr(C)]
pub struct DauxWindowV1 {
    pub size: u32,
    pub api: u32,
    /// HWND / NSView* / X11 Window (as usize) / wl_surface*.
    pub handle: *mut c_void,
    pub display: *mut c_void, // X11 Display* / wl_display*, else null
}

#[repr(C)]
pub struct DauxGuiApiV1 {
    pub size: u32,
    pub _pad0: u32,
    pub is_api_supported: unsafe extern "C" fn(
        p: DauxPluginHandle, api: u32, is_floating: DauxBool,
    ) -> DauxBool,
    pub create: unsafe extern "C" fn(
        p: DauxPluginHandle, api: u32, is_floating: DauxBool,
    ) -> DauxStatus,
    pub destroy: unsafe extern "C" fn(p: DauxPluginHandle),
    pub set_scale: Option<unsafe extern "C" fn(p: DauxPluginHandle, scale: f64) -> DauxStatus>,
    pub get_size: unsafe extern "C" fn(
        p: DauxPluginHandle, width: *mut u32, height: *mut u32,
    ) -> DauxStatus,
    pub can_resize: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxBool,
    pub adjust_size: Option<unsafe extern "C" fn(
        p: DauxPluginHandle, width: *mut u32, height: *mut u32,
    ) -> DauxStatus>,
    pub set_size: unsafe extern "C" fn(
        p: DauxPluginHandle, width: u32, height: u32,
    ) -> DauxStatus,
    pub set_parent: unsafe extern "C" fn(
        p: DauxPluginHandle, window: *const DauxWindowV1,
    ) -> DauxStatus,
    pub show: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,
    pub hide: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxStatus,
    pub reserved: [usize; 6],
}
```

All GUI calls are **[main-thread]**, without exception. Sizes are physical pixels;
`set_scale` reports the HiDPI factor that maps logical to physical units.

### 11.5 Latency, tail, render

```rust
#[repr(C)]
pub struct DauxLatencyApiV1 {
    pub size: u32, pub _pad0: u32,
    /// [main-thread]
    pub get: unsafe extern "C" fn(p: DauxPluginHandle) -> u32,
    pub reserved: [usize; 2],
}

pub const DAUX_TAIL_INFINITE: u32 = u32::MAX;

#[repr(C)]
pub struct DauxTailApiV1 {
    pub size: u32, pub _pad0: u32,
    /// Samples of tail, or `DAUX_TAIL_INFINITE`. [any-thread]
    pub get: unsafe extern "C" fn(p: DauxPluginHandle) -> u32,
    pub reserved: [usize; 2],
}

#[repr(C)]
pub struct DauxRenderApiV1 {
    pub size: u32, pub _pad0: u32,
    pub has_hard_realtime_requirement: unsafe extern "C" fn(p: DauxPluginHandle) -> DauxBool,
    /// [main-thread, inactive only]
    pub set_mode: unsafe extern "C" fn(p: DauxPluginHandle, mode: u32) -> DauxStatus,
    pub reserved: [usize; 2],
}
```

### 11.6 Host-side extensions

```rust
pub const DAUX_LOG_TRACE: u32 = 0;
pub const DAUX_LOG_DEBUG: u32 = 1;
pub const DAUX_LOG_INFO:  u32 = 2;
pub const DAUX_LOG_WARN:  u32 = 3;
pub const DAUX_LOG_ERROR: u32 = 4;
pub const DAUX_LOG_FATAL: u32 = 5;

#[repr(C)]
pub struct DauxHostLogApiV1 {
    pub size: u32, pub _pad0: u32,
    /// MUST be non-blocking and allocation-free when called from the audio thread.
    /// [any-thread]
    pub log: unsafe extern "C" fn(h: DauxHostHandle, level: u32, msg: DauxStrView),
    pub reserved: [usize; 2],
}

#[repr(C)]
pub struct DauxHostParamsApiV1 {
    pub size: u32, pub _pad0: u32,
    /// The plug-in changed a value itself (e.g. from its editor). [main-thread]
    pub changed: unsafe extern "C" fn(h: DauxHostHandle, id: u32, value: f64),
    pub gesture_begin: unsafe extern "C" fn(h: DauxHostHandle, id: u32),
    pub gesture_end: unsafe extern "C" fn(h: DauxHostHandle, id: u32),
    /// Parameter metadata changed; the host must re-read it. [main-thread]
    pub rescan: unsafe extern "C" fn(h: DauxHostHandle, flags: u32),
    pub reserved: [usize; 4],
}

#[repr(C)]
pub struct DauxHostWorkerApiV1 {
    pub size: u32, pub _pad0: u32,
    /// Requests that `on_worker` run off the audio thread. Real-time safe and
    /// non-blocking; returns false when the queue is full. [any-thread]
    pub schedule: unsafe extern "C" fn(h: DauxHostHandle, task_id: u64) -> DauxBool,
    pub reserved: [usize; 2],
}

#[repr(C)]
pub struct DauxHostGuiApiV1 {
    pub size: u32, pub _pad0: u32,
    pub request_resize: unsafe extern "C" fn(h: DauxHostHandle, w: u32, h_px: u32) -> DauxBool,
    pub request_show: unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool,
    pub request_hide: unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool,
    pub closed: unsafe extern "C" fn(h: DauxHostHandle, was_destroyed: DauxBool),
    pub reserved: [usize; 4],
}
```

The host API root:

```rust
#[repr(C)]
pub struct DauxHostApiV1 {
    pub size: u32,
    pub abi_version_major: u32,
    pub abi_version_minor: u32,
    pub _pad0: u32,

    pub name: DauxName,
    pub vendor: DauxName,
    pub version: DauxVersion,

    /// Extension lookup. Callable from any thread, MUST be cheap and lock-free.
    pub get_extension: unsafe extern "C" fn(h: DauxHostHandle, id: DauxStrView) -> *const c_void,

    /// Ask the host to deactivate and reactivate the plug-in. [any-thread]
    pub request_restart: unsafe extern "C" fn(h: DauxHostHandle),
    /// Ask the host to resume calling `process`. [any-thread]
    pub request_process: unsafe extern "C" fn(h: DauxHostHandle),
    /// Ask the host to call `on_main_thread` soon. Real-time safe. [any-thread]
    pub request_callback: unsafe extern "C" fn(h: DauxHostHandle),

    pub is_main_thread: Option<unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool>,
    pub is_audio_thread: Option<unsafe extern "C" fn(h: DauxHostHandle) -> DauxBool>,

    pub reserved: [usize; 8],
}
```

---

## 12. State compatibility

`state_schema_version` in the descriptor is the version the plug-in **writes**. A plug-in
MUST be able to load every schema version it has ever shipped, or return
`DAUX_ERR_VERSION` with no side effects. Partial application of a failed load is a bug:
`load` is atomic from the host's point of view.

---

## 13. Shared-texture extension (optional, DAUx-native)

`com.futureboard.daux.shared-texture/1` lets a plug-in render its editor into a GPU
surface the host composites directly, instead of a nested native child window.

```rust
pub const DAUX_TEXTURE_HANDLE_D3D11_SHARED: u32 = 1; // HANDLE from IDXGIResource1
pub const DAUX_TEXTURE_HANDLE_D3D12_HEAP:   u32 = 2;
pub const DAUX_TEXTURE_HANDLE_IOSURFACE:    u32 = 3;
pub const DAUX_TEXTURE_HANDLE_DMABUF:       u32 = 4;
pub const DAUX_TEXTURE_HANDLE_VULKAN_FD:    u32 = 5;
pub const DAUX_TEXTURE_HANDLE_VULKAN_WIN32: u32 = 6;

#[repr(C)]
pub struct DauxSharedTextureV1 {
    pub size: u32,
    pub handle_kind: u32,
    pub handle: *mut c_void,
    pub format: u32,        // DAUX_TEXTURE_FORMAT_*
    pub width: u32,
    pub height: u32,
    pub row_pitch: u32,
    pub _pad0: u32,
    /// Optional cross-API synchronisation primitive; null when unused.
    pub fence: *mut c_void,
    pub fence_kind: u32,
    pub _pad1: u32,
    pub reserved: [usize; 6],
}
```

Negotiation is mandatory: the host advertises the handle kinds it can import, the plug-in
picks one or declines, and **both sides MUST have a working native-window fallback**.
A plug-in MUST NOT require this extension in order to show an editor.

---

## 14. Stability rules for identifiers

* `DauxPluginDescriptorV1::id` is permanent. Changing it creates a different plug-in and
  silently breaks every saved project that referenced it.
* `DauxParamInfoV1::id` is permanent per plug-in id. A removed parameter's id MUST NOT be
  reused. Renaming is free; re-numbering is not.
* Audio and note port ids are permanent per plug-in id.

---

## 15. Threading

| Class          | Meaning                                                                |
| -------------- | ---------------------------------------------------------------------- |
| `[main-thread]`| The host's main/UI thread. Blocking is tolerated but discouraged.       |
| `[audio-thread]`| A real-time thread. §8 obligations apply with no exceptions.           |
| `[any-thread]` | Must be safe from any thread, including concurrently.                   |

`[audio-thread]` calls for one instance are never concurrent with each other. Calls for
different instances MAY be concurrent, including on different threads, and MAY move
between threads between blocks — no thread-local state may be assumed.

---

## 16. Ownership and lifetime

### 16.1 Object lifetime

```
module (dynamic library)
└── entry (static, immortal while loaded)
    └── factory                      created by host, destroyed by host
        └── plug-in instance         created via factory, destroyed via its own api
            ├── extension tables     valid while the instance lives
            └── editor               created/destroyed via daux.gui/1, may repeat
```

The host MUST NOT unload the library while any factory or instance exists. A plug-in MUST
NOT retain the `DauxHostV1` pointer after `destroy_factory` returns.

### 16.2 Memory

No allocation crosses the boundary. Every buffer is either owned by the caller and filled
by the callee, or owned by the callee and read by the caller within a single call. There
is no cross-module `free`, ever.

### 16.3 Buffers during `process`

All pointers inside `DauxProcessV1` — audio, events, transport, SysEx payloads — are
borrowed for exactly the duration of that call.

---

## 17. Panics and faults

A Rust panic unwinding across `extern "C"` is undefined behaviour. Every exported ABI
function in a DAUx binary MUST be wrapped so that:

1. Unwinding is caught at the boundary.
2. The failure is converted to `DAUX_ERR_PANIC` (or `DAUX_PROCESS_ERROR` from `process`).
3. The instance marks itself poisoned and refuses further work with `DAUX_ERR_INVALID_STATE`.
4. Debug builds MAY emit diagnostics via `daux.host.log/1`; release builds MUST stay silent
   and deterministic.

Hosts MUST treat a poisoned instance as unloadable-but-safe, never as a reason to abort.

---

## 18. Conformance checklist

A binary conforms to DAUx ABI v1 when it:

- [ ] exports `daux_plugin_entry_v1` and nothing else with a `daux_` prefix;
- [ ] fills `size`, `abi_version_*`, `magic` correctly and zeroes all reserved fields;
- [ ] never unwinds across the boundary;
- [ ] never allocates, locks, or blocks inside `process`;
- [ ] tolerates any `frame_count` in `1 ..= max_block_size`;
- [ ] returns `DAUX_ERR_UNSUPPORTED` (not a crash) for every extension it does not implement;
- [ ] keeps parameter and port ids stable across versions;
- [ ] survives a host that provides no transport, no GUI, and no optional host extensions.
