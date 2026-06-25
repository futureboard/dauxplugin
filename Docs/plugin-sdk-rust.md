# DAUx Plugin SDK — Rust

Two crates in [`daux/Wrappers/Rust`](../Wrappers/Rust):

- **`daux-plugin-sys`** — raw, hand-written FFI bindings to the C ABI. `unsafe`,
  `#[repr(C)]` structs that mirror `daux/Core/include` field-for-field. No build
  dependencies.
- **`daux-plugin`** — the safe, ergonomic layer: the `DauxPlugin` trait, safe
  `Buffers` / `ProcessContext` views, and the `daux_export_plugin!` macro that
  generates the C entry point / factory / vtable.

## Writing a plugin

```rust
use daux_plugin::*;

pub struct GainPlugin { gain_db: f64 }

impl DauxPlugin for GainPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            id: "com.example.gain".into(),
            name: "Gain".into(),
            vendor: "Me".into(),
            caps: Caps::STATE,
            ..Default::default()
        }
    }
    fn parameters() -> Vec<ParamInfo> {
        vec![ParamInfo::new(1, "Gain", "dB", -60.0, 12.0, 0.0)]
    }
    fn new() -> Self { GainPlugin { gain_db: 0.0 } }

    fn process(&mut self, _ctx: &ProcessContext, b: &mut Buffers) {
        let g = 10f64.powf(self.gain_db / 20.0) as f32;
        let frames = b.frames();
        for bus in 0..b.output_bus_count() {
            for ch in 0..b.output_channels(bus) {
                let input = b.input(bus, ch).map(|s| s.to_vec());
                if let Some(out) = b.output(bus, ch) {
                    for i in 0..frames {
                        out[i] = input.as_ref().and_then(|s| s.get(i)).copied().unwrap_or(0.0) * g;
                    }
                }
            }
        }
    }

    fn get_param(&self, _id: u32) -> f64 { self.gain_db }
    fn set_param(&mut self, _id: u32, v: f64) { self.gain_db = v.clamp(-60.0, 12.0); }
}

daux_export_plugin!(GainPlugin);
```

`Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
daux-plugin = { path = ".../daux-plugin" }
```

## Building

```sh
cargo build --release
```

On Windows this produces `target/release/<name>.dll`. Rename or copy it to
`<name>.dauxplug` for the DAUx loader/scanner:

```powershell
Copy-Item target/release/my_plugin.dll my_plugin.dauxplug
```

(`DAUxHost --mode=headless --plugin=...` can also load the `.dll` directly by
path; only the directory scanner filters on the `.dauxplug` extension.)

## How the bridge works

`daux_export_plugin!(T)` expands to an anonymous const block that:

- builds the descriptor / vtable / factory once into `OnceLock`s (wrapped in
  `AbiStatic` to satisfy `Sync` for the pointer-bearing POD structs),
- monomorphizes the generic `export::*::<T>` thunks as the vtable's
  `extern "C"` function pointers,
- defines the single `#[no_mangle] pub extern "C" fn daux_plugin_entry`.

Each instance is a `Box<T>`; the native handle is the raw box pointer. All
`unsafe` lives in `daux-plugin`/`daux-plugin-sys`, not your plugin.

## TODO / roadmap

- GPUI/DAUx editor: implement a `daux_editor_vtable` backed by a GPUI view,
  expose it through the trait, and set `Caps::EDITOR`. Hosting is via
  `DAUxHost --mode=editor`.
- Per-parameter normalized↔plain curves (currently linear; vtable entries left
  `None`).
- float64 processing.
