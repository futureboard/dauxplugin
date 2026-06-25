# DAUx Component Model

DAUx separates plugin responsibilities the same way mature plugin SDKs do, but
with a **DAUx-native C ABI** — not a copy of VST3/CLAP/AU APIs.

## Roles

| Role | Thread | Responsibility |
|------|--------|----------------|
| **Processor** | Realtime audio | `prepare`, `process`, bus activation, events, automation consumption |
| **Controller** | UI / control | Parameters, editor/GUI, presets, state, host notifications |
| **Component** | Owner | Pairs one processor + one controller for a plugin class |

Legacy plugins still expose a single `daux_plugin_factory` + `daux_plugin_vtable`.
Modern plugins may additionally export `daux_get_plugin_factory()` with one or
more `DAUxPluginClassInfo` entries.

## Headers

- `DAUx/Component/Processor.h` — realtime DSP surface
- `DAUx/Component/Controller.h` — parameters, GUI, programs
- `DAUx/Component/Component.h` — `create` / `get_processor` / `get_controller`
- `DAUx/Component/ProcessData.h` — buses, events, automation queues

## Compatibility

`daux_plugin_entry` and `daux_process_data` remain the supported path for
existing Host, Rust, and .NET examples. The component split is additive.
