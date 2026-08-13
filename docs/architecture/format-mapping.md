# Format capability mapping

One source tree, three binaries. This document states exactly what survives the trip to each
format, what gets translated, and what is lost — because the failure mode that matters is not
"this doesn't work", it's "this quietly did something else".

**Nothing is silently dropped.** Every adapter implements
`compatibility_report(&PluginDescriptor) -> Vec<CompatibilityWarning>`, and `daux build`
prints the warnings for each format it exports. If your plug-in declares MIDI 2.0 input and
you export a VST3, you find out at build time, not from a bug report.

## Legend

| | |
| --- | --- |
| **native** | Expressed directly; no translation loss |
| **mapped** | Translated to the format's equivalent; semantics preserved |
| **lossy** | Translated with documented loss of fidelity or precision |
| **fallback** | Not expressible; the adapter substitutes a working alternative |
| **—** | Not available; reported as a warning at build time |

## Core model

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Audio effect | native | mapped | mapped |
| Instrument | native | mapped | mapped |
| MIDI effect (no audio) | native | lossy¹ | mapped |
| Analyzer (no output) | native | mapped | mapped |
| Variable block size | native | native | native |
| `f32` processing | native | native | native |
| `f64` processing | native | mapped | fallback² |
| In-place processing | native | native | native |
| Offline / realtime mode switch | native | mapped | mapped |
| Multiple plug-ins per binary | native | native | native |

¹ VST3 has no first-class MIDI-effect category; the adapter exports an effect with zero audio
buses, which some hosts categorise oddly.
² CLAP processes `f32`; a `f64` plug-in is driven through a converting wrapper, and the
warning names the extra conversion cost.

## Buses

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Arbitrary channel counts | native | mapped | native |
| Named buses | native | native | native |
| Sidechain input | native | native | native |
| Multiple output buses | native | native | native |
| Optional / deactivatable buses | native | native | mapped |
| **Dynamic bus count at runtime** | native | fallback³ | fallback³ |
| Surround / Atmos layouts | native | mapped⁴ | mapped |
| Ambisonics | native | mapped⁴ | mapped |
| CV / control-rate buses | native | — | mapped |

³ Both formats negotiate bus configuration while inactive. A plug-in that wants to add a bus
mid-session gets a `request_restart` instead; the adapter warns.
⁴ Mapped onto VST3 `SpeakerArrangement` bit masks. Layouts with no VST3 equivalent become
discrete channel sets and the warning names the layout.

## Parameters

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Stable ids | native | mapped⁵ | native |
| Plain (real-world) values | native | lossy⁶ | native |
| Sample-accurate automation | native | mapped | native |
| Per-note / polyphonic modulation | native | lossy⁷ | native |
| Modulation separate from automation | native | — | native |
| Begin / end gesture | native | native | native |
| Stepped and enum parameters | native | native | native |
| Parameter groups | native | native | native |
| Text ↔ value conversion | native | native | native |
| Meters / read-only outputs | native | mapped | mapped |
| Adding parameters in a later version | native | mapped | native |

⁵ VST3 parameter ids are `u32` tags, which matches, but VST3 hosts key automation on index in
some legacy paths; the adapter keeps index order stable across versions and warns if you
reorder.
⁶ **The important one.** VST3 parameters are normalised `[0, 1]`. The adapter converts in both
directions using the parameter's range and curve. If you change a curve between versions, VST3
automation lanes shift while AXT and CLAP lanes do not. Changing a shipped parameter's range or
curve is therefore a breaking change for VST3 users specifically.
⁷ VST3 expresses per-note control through note expression, which covers the standard
dimensions but not arbitrary per-note parameter modulation.

## Events

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Sample-accurate notes | native | native | native |
| Note ids / voice tracking | native | native | native |
| Note expression (pitch, pressure, timbre…) | native | native | mapped |
| Note end → host | native | mapped | native |
| MIDI 1.0 in / out | native | mapped | native |
| **MIDI 2.0 / UMP** | native | lossy⁸ | native |
| SysEx | native | native | native |
| Transport events mid-block | native | fallback⁹ | fallback⁹ |
| Custom / vendor events | native | — | — |

⁸ VST3 has no UMP transport. MIDI 2.0 messages are downconverted to MIDI 1.0 (16-bit velocity
→ 7-bit, using min-center-max scaling) or to note expression where a mapping exists.
Per-note controllers beyond the standard set are dropped, and the warning says so.
⁹ Both formats deliver one transport snapshot per block. A mid-block tempo jump is applied at
the block boundary; sample-accurate locate points are lost.

## State

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Versioned state + migration | native | native | native |
| Separate UI state | native | mapped¹⁰ | mapped¹⁰ |
| Deterministic byte-for-byte output | native | native | native |
| State portable between formats | native | native | native |

The state blob is produced by `daux-state` and is **identical across all three formats**. A
preset saved from the VST3 build loads into the AXT build. That is a deliberate guarantee, and
`tests/harness/tests/state.rs` enforces it.

¹⁰ VST3 splits component and controller state; CLAP has one blob. The adapter concatenates
with a length prefix so both round-trip.

## GUI

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Embedded native window | native | native | native |
| HiDPI scale factor | native | native | native |
| Host-driven resize | native | native | native |
| Plug-in-requested resize | native | native | native |
| Keyboard focus / IME | native | mapped | mapped |
| **Shared-texture compositing** | native | fallback | fallback |
| Multiple simultaneous editors | native | — | — |
| Headless (no GUI at all) | native | native | native |

Shared textures fall back to an embedded native window automatically. The plug-in code does
not change; only the presentation mode negotiated at editor creation differs.

## Host services

| Capability | AXT | VST3 | CLAP |
| --- | --- | --- | --- |
| Structured logging | native | fallback¹¹ | native |
| Worker thread scheduling | native | fallback¹² | native |
| Latency reporting (static) | native | native | native |
| Latency changes at runtime | native | mapped | native |
| Tail length | native | native | native |
| Main-thread callback request | native | mapped | native |
| Timer | native | mapped | native |
| Restart request | native | native | native |
| **Custom DAUx host extensions** | native | — | — |
| **Sandbox / out-of-process** | native | host-provided | host-provided |

¹¹ VST3 has no logging interface; the adapter routes to a no-op sink, or to stderr in debug
builds only.
¹² VST3 has no worker pool; the adapter runs a small owned thread pool per module and says so.

## Rules the adapters follow

1. **Never fake a capability.** If the host cannot supply transport, `transport()` returns
   `None`. The adapter does not invent a tempo of 120.
2. **Never silently downgrade.** Every lossy or unavailable mapping produces a
   `CompatibilityWarning` at build time, naming the capability and the consequence.
3. **Never leak format types inward.** `daux-core` has no VST3 or CLAP types in it, and the
   dependency graph makes that structural rather than aspirational.
4. **Prefer a working fallback to an error.** A plug-in that wants a shared-texture editor in
   a VST3 host gets a normal embedded window, not a failure to load.
