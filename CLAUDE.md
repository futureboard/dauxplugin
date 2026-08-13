# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

DAUxPlug is a **100% Rust** audio plug-in platform and SDK. A plug-in is written once
against `daux-plugin` and exported as:

- **`.axt`** — DAUx Audio Extension, the native format, over the DAUx C ABI
- **VST3** — compatibility adapter, pure Rust, no Steinberg C++ SDK
- **CLAP** — compatibility adapter, pure Rust

There is no C++ anywhere in this project, and no C++ toolchain is needed to build or use
it. The old C++ DAUxPlug is legacy/reference only; do not port its architecture.

`DAUxPlug_Pure_Rust_Rearchitecture_Superprompt.md` in the repo root is the original design
brief. `docs/specifications/abi-v1.md` is the binary contract and wins over any code.
`docs/architecture/crate-contracts.md` fixes the cross-crate public API surface.

## Hard rules

These are architectural invariants, not style preferences. Breaking one is a bug even if
it compiles.

1. **The audio thread never allocates.** No `Vec::push`, `Box::new`, `String`, `format!`,
   `collect`, `to_owned`, `Mutex::lock`, file I/O, logging that formats, `dlopen`, thread
   spawn, GUI call, or unbounded loop inside `process` or anything it calls. Preallocate in
   `prepare`/`activate`. When in doubt, check `docs/architecture/realtime.md`.
2. **The Rust ABI never crosses a dynamic library boundary.** Anything reachable from an
   exported symbol is `#[repr(C)]` + `extern "C"` + opaque handles + versioned function
   tables. No `Vec`, `String`, `&T`, trait objects, generics, or Rust enums in ABI types.
3. **Panics never unwind across FFI.** Every exported function wraps its body in
   `catch_unwind` and converts failure to a status code.
4. **Core crates have zero external dependencies**: `daux-abi`, `daux-rt`, `daux-audio`,
   `daux-midi`, `daux-events`, `daux-parameter`, `daux-state`, `daux-transport`,
   `daux-core`. Adding one to these is a design change, not a convenience.
5. **The DAUx model is format-neutral.** VST3 and CLAP concepts must not leak into
   `daux-core`. Translation lives in `crates/daux-format-*`.
6. **No global mutable state, no singletons.** Hundreds of instances of the same plug-in
   must coexist in one process.
7. **Every `unsafe` block has a `// SAFETY:` comment** explaining pointer ownership,
   lifetime, alignment, thread, and mutation assumptions.
8. **Parameter ids and plug-in ids are permanent.** Renaming is free; renumbering silently
   corrupts users' saved projects.
9. **GUI lifetime is independent of DSP lifetime.** An editor may open and close many
   times while the processor keeps running; closing an editor must never touch DSP state.

## Layout

```
crates/
  daux-abi              C ABI transcription of docs/specifications/abi-v1.md   [zero deps]
  daux-rt               lock-free queues, bounded buffers, thread markers      [zero deps]
  daux-audio            planar buffer views, channel layouts, bus topology     [zero deps]
  daux-midi             MIDI 1.0 + MIDI 2.0/UMP                                [zero deps]
  daux-events           sample-accurate, format-neutral event model            [zero deps]
  daux-parameter        typed params, ranges, curves, smoothing, gestures      [zero deps]
  daux-state            versioned state container + migration chain            [zero deps]
  daux-transport        transport / musical timeline                           [zero deps]
  daux-dsp              small, focused DSP + runtime SIMD dispatch
  daux-host-services    explicit host service traits (RT-safe subset split out)
  daux-core             the plug-in object model: plugin/processor/controller/factory
  daux-graphics         framework-neutral editor abstraction (no GPU/UI deps)
  daux-bundle           .axt layout, manifest.json / Info.plist, resource resolution
  daux-protocol         sandbox wire types          daux-ipc  transports + loopback
  daux-plugin-api       safe authoring API          daux-plugin-macros  derives
  daux-plugin           the facade developers depend on
  daux-format-axt       native export over the DAUx ABI
  daux-format-vst3      VST3 export (pure Rust COM)
  daux-format-clap      CLAP export (pure Rust CLAP ABI)
  daux-runtime          host-side loader: bundle → library → factory → instance
  daux-scan             scanner with caching and crash isolation
  daux-host             in-process test/preview harness
  daux-cli              the `daux` CLI
  daux-graphics-{egui,wgpu,gl,gpui}   optional backends, excluded from default-members
examples/               gain, synth, midi-effect, multi-plugin-bundle, gain-egui, analyzer-gpui
tests/harness           cross-crate abi / realtime / bundle / scanner suites
docs/                   specifications (normative), architecture, guides
```

Dependency direction is enforced by the root `Cargo.toml`; it is acyclic and deliberate.
`daux-plugin` → `daux-plugin-api` → `daux-core` → foundation crates. Adapters and the
runtime sit beside the model, never inside it.

## Commands

```bash
cargo build                     # default-members: fast, no GPU/UI dependency trees
cargo test                      # same set
cargo build --workspace         # adds egui/wgpu/gl/gpui backends and their examples
cargo clippy --workspace --all-targets
cargo fmt --all

cargo run -p daux-cli -- inspect target/daux/release/axt/Gain.axt
cargo run -p daux-cli -- validate target/daux/release/axt/Gain.axt
cargo run -p daux-cli -- scan
```

Heavy GUI crates are **workspace members but not default members** on purpose: plain
`cargo build`/`cargo test` must stay fast and must never require a GPU. Do not "fix" this
by adding them to `default-members`.

**On Windows, run cargo from PowerShell, not Git Bash.** In a Git Bash shell,
`/usr/bin/link` shadows MSVC's `link.exe` and every test binary fails to link with
`/usr/bin/link: missing operand`. Compilation succeeds; only linking fails. It is a PATH
artefact, not a code problem.

## Conventions

- Public DAUx concepts are spelled `Daux…` in Rust (never `DAUX…`, never `Dauxx…`);
  user-facing prose says "DAUx" and "AXT".
- Doc comments on anything callable from the audio thread carry `[audio-thread]`,
  `[main-thread]` or `[any-thread]`, matching abi-v1 §15.
- Errors: rich Rust errors internally, stable integer status codes at ABI boundaries.
  Never allocate an error message on the audio thread.
- Tests that assert real-time behaviour use `daux_rt::AllocGuard`.
- Prefer `cargo check -p <crate>` while iterating; the workspace is large.

## When adding a crate

1. Add it to both `members` and (unless it drags in a GPU/UI stack) `default-members`.
2. Add it to `[workspace.dependencies]` with `version` **and** `path`.
3. Give it `[lints] workspace = true`.
4. Record its cross-crate surface in `docs/architecture/crate-contracts.md`.
