# Architecture overview

DAUxPlug is built around one idea: **the plug-in model is not a plug-in format.**

Every other decision falls out of that. The model — what a plug-in *is*, what it can
express, how it talks to a host — lives in pure, safe Rust with no idea that VST3 or CLAP
exist. Formats are adapters bolted on at the edge. The native format, `.axt`, gets to be
richer than the others precisely because it is not the thing the model was designed around.

```
                       ┌─────────────────────────────────────┐
     you write ───────►│           your plug-in              │
                       │   impl DauxPlugin / DauxProcessor   │
                       └──────────────────┬──────────────────┘
                                          │  safe Rust, no unsafe, no ABI
                       ┌──────────────────▼──────────────────┐
                       │            daux-plugin              │  facade + prelude + macros
                       │            daux-plugin-api          │
                       └──────────────────┬──────────────────┘
                                          │
                       ┌──────────────────▼──────────────────┐
                       │             daux-core               │  the model:
                       │  Plugin · Processor · Controller    │  format-neutral,
                       │  Factory · Descriptor · Context     │  allocation-aware
                       └──┬───────────────┬───────────────┬──┘
                          │               │               │
        ┌─────────────────▼───┐  ┌────────▼────────┐  ┌───▼──────────────┐
        │  daux-format-axt    │  │ daux-format-vst3│  │ daux-format-clap │
        └─────────┬───────────┘  └────────┬────────┘  └───┬──────────────┘
                  │                       │               │
        ┌─────────▼───────────┐  ┌────────▼────────┐  ┌───▼──────────────┐
        │  DAUx C ABI (v1)    │  │   VST3 COM ABI  │  │    CLAP C ABI    │
        │  daux_plugin_entry_v1│  │ GetPluginFactory│  │   clap_entry     │
        └─────────┬───────────┘  └─────────────────┘  └──────────────────┘
                  │
        ┌─────────▼────────────────────────────────────────────────────────┐
        │  daux-runtime · daux-bundle · daux-scan   (host side of .axt)    │
        └──────────────────────────────────────────────────────────────────┘
```

---

## The layers

### Foundation — zero dependencies, no opinions

`daux-abi`, `daux-rt`, `daux-audio`, `daux-midi`, `daux-events`, `daux-parameter`,
`daux-state`, `daux-transport`. These crates have **no external dependencies at all**, by
architectural rule. They are the vocabulary: a buffer view, a bounded queue, a parameter, a
MIDI 2.0 packet, a versioned state blob.

The rule is not asceticism. A plug-in binary that a user loads into their DAW should
contain what it needs and nothing else, and a dependency added at the bottom of this stack
is a dependency in every plug-in anyone ever ships with this SDK.

`daux-abi` is special: it is a transcription of
[`abi-v1.md`](../specifications/abi-v1.md) and contains no logic. Both sides of the FFI
boundary — the plug-in's export adapter and the host's loader — compile against it, but
neither trusts the other to have compiled the same version. That's what `size`,
`abi_version` and the `reserved` tails are for.

### Model — `daux-core`

The object model. A plug-in is three collaborating pieces with different thread lives:

- **`DauxProcessor`** — the audio thread. Owns DSP state, obeys
  [the real-time rules](realtime.md) absolutely.
- **`DauxController`** — the main thread. Owns parameter metadata, state serialisation,
  and the conversation with the host.
- **the editor** — created and destroyed at will, sharing only an `Arc<Params>` and some
  bounded queues with the other two.

They are separate because their lifetimes are genuinely different, and a design that
couples them (as many SDKs do) forces the DSP to stop when a window closes.

`ProcessContext` is the only thing `process` gets, and it hands out only `RtHostServices` —
the subset of host functionality that is provably real-time safe. The blocking services
aren't hidden behind a runtime check; they simply aren't reachable from there.

### Adapters — `daux-format-*`

Each adapter is a translation layer and nothing more. It:

1. wraps every exported symbol in `catch_unwind` so a panic can never unwind into a host;
2. maps DAUx concepts onto the format's concepts, and reports what it cannot map through
   `compatibility_report`, which `daux build` prints at export time;
3. keeps its format's types entirely to itself.

`daux-core` contains zero references to VST3 or CLAP. That is checked by inspection and by
the dependency graph in the root `Cargo.toml`: `daux-core` cannot depend on the adapters,
because the arrow points the other way.

### Native format — `.axt`

The native path is not "another adapter". It is the format that gets to expose what the
model can actually do:

| Capability                      | `.axt`  | VST3          | CLAP          |
| ------------------------------- | ------- | ------------- | ------------- |
| Sample-accurate automation      | native  | mapped        | mapped        |
| Dynamic bus configuration       | native  | mapped        | mapped        |
| MIDI 2.0 / UMP                  | native  | partial       | mapped        |
| Shared-texture GPU editor       | native  | fallback      | fallback      |
| Custom host extensions          | native  | limited       | limited       |
| Sandbox / out-of-process        | native  | host-provided | host-provided |

The rule is that nothing is *silently* dropped. Where a capability can't be expressed,
the adapter says so at build time rather than at 2 a.m. in someone's session.

### Host side — `daux-bundle`, `daux-runtime`, `daux-scan`

Bundles are untrusted input. `daux-bundle` parses `manifest.json` or `Info.plist` with
bounded allocations, resolves resources through a namespace that cannot escape the bundle
root, and never panics on malformed data. `daux-runtime` loads the library with scoped
dependency directories — never by mutating `PATH` or `LD_LIBRARY_PATH` — validates the ABI
header before calling anything, and keeps the `Library` alive inside an `Arc` that every
derived handle holds, so use-after-unload is impossible by construction rather than by
discipline.

Scanning never instantiates DSP. A descriptor is cheap by design so that a host with 3,000
plug-ins installed can enumerate them without loading 3,000 GPU contexts.

---

## Decisions worth knowing about

**Plain values, not normalised values, cross every boundary.** Normalisation is a plug-in
implementation detail. If the ABI carried normalised values, changing a parameter's curve
in v2 would silently rewrite every automation lane in every saved project. Plain values
make the curve a private concern.

**No memory ever crosses the module boundary.** Every buffer is either caller-owned and
callee-filled, or callee-owned and read within a single call. State save/load goes through
a host-owned stream. There is no cross-module `free`, so there is no allocator contract to
get wrong.

**Strings written by the callee use fixed-size UTF-8 buffers.** This trades a truncation
limit for the complete elimination of a class of lifetime bugs. Input strings are borrowed
views valid for the duration of the call. There is exactly one rule for each direction.

**Optional everything.** Every host service, every plug-in extension, every graphics
presentation mode is negotiated. A plug-in must work in a host that provides no transport,
no GUI, no worker pool, and no optional extensions at all — because such hosts exist, and
because the sandbox path is one of them.

**Sandboxing was designed in, not bolted on.** `daux-protocol` and `daux-ipc` exist from
the start, and the in-process path is deliberately expressible as a degenerate case of the
out-of-process one: bounded queues, explicit control messages, no shared Rust types.
The transports are incomplete in v1; the architecture that would make them impossible was
avoided from day one. See [sandboxing.md](sandboxing.md).

**Graphics is three orthogonal axes, not one enum.** UI framework (egui, GPUI, custom),
rendering backend (wgpu, OpenGL, software) and presentation mode (native window, embedded
surface, shared texture, external window) vary independently. Collapsing them — as
"backend" does in most SDKs — is what makes GUI code impossible to port later. See
[graphics.md](graphics.md).

---

## What this costs

Being honest about the trade-offs:

- **More crates than a monolith.** The dependency graph is the enforcement mechanism for
  most of the rules above, so it has to be granular enough to express them.
- **Fixed-size metadata strings truncate.** A 300-character plug-in description will lose
  its tail. That is a deliberate trade against a whole category of FFI lifetime bugs.
- **Two parameter representations** (plain and normalised) means two conversion points and
  a round-trip test suite to keep them honest.
- **The adapters duplicate concepts.** VST3 and CLAP each re-express buses, parameters and
  events. That duplication is the price of not letting either format define the model.
