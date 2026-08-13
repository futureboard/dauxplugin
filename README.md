# DAUxPlug

**A 100% Rust audio plug-in platform and SDK.** Write a plug-in once, ship it as a native
DAUx Audio Extension (`.axt`), a VST3 and a CLAP — with no C++ anywhere in the toolchain.

```rust
use daux_plugin::prelude::*;

#[derive(DauxParams)]
struct GainParams {
    #[param(id = 1, name = "Gain", range = -60.0..=12.0, unit = "dB", default = 0.0)]
    gain: FloatParam,
}

#[derive(DauxPlugin, Default)]
#[plugin(
    id = "studio.futureboard.gain",
    name = "Gain",
    vendor = "Futureboard Studio",
    version = "1.0.0",
    category = "effect"
)]
struct Gain {
    params: Arc<GainParams>,
    processor: GainProcessor,
}

daux_plugin::export_plugin!(SingleFactory<Gain>);
```

```bash
daux build --release --formats axt,vst3,clap
```

---

## Why

Existing plug-in SDKs make you adopt a C++ object model, a framework's threading
assumptions, and a GUI toolkit you didn't choose — and then hope that nothing in your
audio callback allocates. DAUxPlug starts from the other end:

- **Real-time safety is a design constraint, not a code review note.** The audio-thread API
  is built out of bounded, lock-free primitives, and the test harness can fail a build that
  allocates inside `process`.
- **The Rust ABI never crosses a dynamic library boundary.** The native format is defined by
  a versioned, `#[repr(C)]`, extensible C ABI ([`abi-v1.md`](docs/specifications/abi-v1.md)),
  so a host and a plug-in built by different compilers still interoperate.
- **The plug-in model is format-neutral.** VST3 and CLAP are export adapters. Neither shapes
  the core, and `.axt` is free to expose capabilities the others can't express — GPU
  shared-texture editors, sandbox services, MIDI 2.0, custom host extensions.
- **GUI and DSP are decoupled.** Editors open and close as often as the user likes; the
  processor never notices.

## Status

Version `0.1.0`, pre-release. The ABI is specified and stable-in-shape; the surrounding
tooling is young. See [`docs/`](docs/) for what is normative and what is still moving.

| Component            | State                                                     |
| -------------------- | --------------------------------------------------------- |
| DAUx ABI v1          | Specified, implemented                                     |
| `.axt` bundle + CLI  | Implemented (build, bundle, validate, inspect, scan)       |
| Parameters, state, events, MIDI 1.0/2.0 | Implemented                             |
| VST3 export          | Implemented, pure Rust                                     |
| CLAP export          | Implemented, pure Rust                                     |
| egui / wgpu editors  | Implemented, optional                                      |
| GPUI editors         | Experimental                                               |
| Sandboxed hosting    | Architecture + protocol in place, transports incomplete    |

## Getting started

```bash
cargo install --path crates/daux-cli     # the `daux` CLI
daux new my-plugin --template effect
cd my-plugin
daux build --release --formats axt,clap
daux inspect target/daux/release/axt/my-plugin.axt
```

Guides live in [`docs/guides/`](docs/guides/): [your first
plug-in](docs/guides/first-plugin.md), [parameters](docs/guides/parameters.md), [audio
processing](docs/guides/audio-processing.md), [egui editors](docs/guides/gui-egui.md),
[exporting to VST3](docs/guides/export-vst3.md) and [CLAP](docs/guides/export-clap.md).

## Workspace

```
crates/daux-plugin          ← the crate you depend on
       daux-plugin-api      safe authoring API + prelude
       daux-core            format-neutral plug-in model
       daux-abi             the C ABI, transcribed from the spec
       daux-rt              lock-free, bounded, real-time primitives
       daux-audio  -events  -midi  -parameter  -state  -transport  -dsp
       daux-graphics        framework-neutral editor abstraction
       daux-format-axt      -vst3  -clap        export adapters
       daux-bundle  -runtime  -scan  -host  -cli   host side and tooling
       daux-protocol  -ipc   out-of-process hosting
examples/                   gain · synth · midi-effect · multi-plugin · egui · gpui
```

Foundation crates carry **zero external dependencies**. Heavy GUI backends are optional and
excluded from `default-members`, so `cargo build` and `cargo test` never pull a GPU stack.

## Documentation

- [ABI specification](docs/specifications/abi-v1.md) — normative binary contract
- [AXT bundle specification](docs/specifications/axt-v1.md)
- [Manifest specification](docs/specifications/manifest-v1.md)
- [Architecture overview](docs/architecture/overview.md) ·
  [real-time rules](docs/architecture/realtime.md) ·
  [threading](docs/architecture/threading.md) ·
  [graphics](docs/architecture/graphics.md) ·
  [sandboxing](docs/architecture/sandboxing.md)
- [Crate contracts](docs/architecture/crate-contracts.md) — the cross-crate API surface

## Building

```bash
cargo build                 # fast path: no GPU or UI dependencies
cargo test
cargo build --workspace     # everything, including egui/wgpu/gpui backends
cargo clippy --workspace --all-targets
```

Requires Rust 1.85+ (edition 2024). No C++ compiler, no vendored SDK, no submodules.

## License

MIT OR Apache-2.0, at your option. VST3 and CLAP are trademarks/specifications of their
respective owners; the adapters here are independent, clean-room Rust implementations of
the published binary interfaces.
