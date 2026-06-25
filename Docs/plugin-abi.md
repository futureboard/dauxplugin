# DAUx Plugin ABI

The **DAUx Plugin ABI** is a stable **C ABI** that defines the boundary between
a host (Futureboard, DAUxHost) and a plugin (`.dauxplug`). It is deliberately
small, POD/handle/function-pointer based, and free of any STL / Rust / .NET
types so that plugins and hosts written in different languages and compiled with
different toolchains interoperate.

Headers live in [`daux/Core/include`](../Core/include):

| Header | Purpose |
|---|---|
| `daux_export.h` | Calling convention (`cdecl`) + export macros |
| `daux_types.h`  | POD types, enums, result codes, descriptor, process data |
| `daux_host.h`   | Host callback table passed to the plugin |
| `daux_editor.h` | Editor (GUI) interface |
| `daux_plugin.h` | Entry point, factory, per-instance vtable |

> `daux_core.hpp` is **not** part of the ABI. It is a C++ host-side helper
> (RAII loader, etc.). Plugins must never include it.

## Versioning

`DAUX_ABI_VERSION` is a packed `uint32` (`0xMMmmpppp`). The **major** version is
the compatibility gate:

- The host calls `daux_plugin_entry(DAUX_ABI_VERSION)`. A plugin returns `NULL`
  if the host's major version differs from its own.
- The host checks `factory->abi_version` and rejects a major mismatch
  (`DAUX_ERR_ABI_MISMATCH`).

Minor/patch bumps must be backward compatible: only append fields to the tail of
structs (the `reserved[...]` arrays exist for this), never reorder or resize
existing fields. Each vtable/struct carries `struct_size` for forward-compat
checks.

## Loading model

A `.dauxplug` is a dynamic library (a PE DLL on Windows) exporting exactly one
symbol:

```c
const daux_plugin_factory* daux_plugin_entry(uint32_t host_abi_version);
```

Host sequence:

1. `LoadLibrary` / `dlopen` the file.
2. Resolve `"daux_plugin_entry"`.
3. Call it with `DAUX_ABI_VERSION`.
4. Validate `factory->abi_version` (major must match).
5. Read `factory->get_descriptor()` — cheap, no instance (used by DAUxScan).
6. `factory->create(host, &instance)` to instantiate.
7. Use the instance via `instance.vtable` (see lifecycle below).
8. `factory->destroy(instance.handle)` when done.

## Packaging: single-file vs bundle

A `.dauxplug` may be **either**:

- **A single file** — a self-contained dynamic library with no external
  dependencies. This is the default and is fully achievable for C++ (static
  CRT), Rust (cdylib statically links std + crates), and headless .NET
  (NativeAOT statically links the runtime).

- **A bundle directory** — `Name.dauxplug/` (VST3 / macOS-bundle style) with a
  fixed internal layout. Use this when the plugin's toolkit ships dynamic libs
  that can't practically be statically linked — e.g. a .NET/Avalonia editor
  needs `libSkiaSharp.dll` / `libHarfBuzzSharp.dll`. Bundling keeps those inside
  the plugin instead of polluting the host directory.

  ```
  Name.dauxplug/
    Exec/Name.dll       entry module (or <exec> named in manifest.xml)
    Library/*.dll       private native dependencies (Skia, HarfBuzz, ...)
    Resources/*         assets (icons, etc.)
    manifest.xml        optional metadata + entry override
  ```

  Entry resolution (see `daux_core` `resolve_module_path`): `manifest.xml`
  `<exec>` under `Exec/`, else `Exec/Name.dll` (Name = bundle stem), else any
  `*.dll` under `Exec/`, else a legacy flat `Name.dll`. The host registers
  `Library/` (+ `Exec/` + root) on the DLL search path (`AddDllDirectory` +
  `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR`) so private deps resolve — at load time and
  for later dynamic P/Invoke.

  `manifest.xml` is optional metadata (lets a host scan/validate without loading
  native code); the authoritative values still come from the descriptor:

  ```xml
  <daux-plugin abi="1.0.0">
    <id>com.daux.examples.gain.dotnet</id>
    <name>DAUx Gain (.NET)</name>
    <vendor>DAUx</vendor>
    <version>0.1.0</version>
    <category>effect</category>
    <exec>daux_gain_dotnet.dll</exec>
  </daux-plugin>
  ```

The host loader and DAUxScan accept both forms transparently. (`Resources/` is a
reserved convention; surfacing the bundle path to the plugin for runtime asset
access is a TODO.)

## Descriptor

`daux_plugin_descriptor` (one per plugin) carries id, name, vendor, packed
version, category, capability flags (`DAUX_CAP_*`), and bus counts. All strings
are inline, fixed-size, NUL-terminated UTF-8 buffers so the host never manages
plugin-owned string lifetimes.

## Lifecycle (per instance)

```
create
  └─ prepare(sample_rate, max_block_size)   // (re)called on format change
       └─ activate
            └─ process(...)   // realtime thread, repeated
            └─ reset          // clear tails
       └─ deactivate
destroy
```

`process()` runs on the realtime audio thread and must not allocate or block.
Every other call happens on a non-realtime thread and never concurrently with
`process()`.

## Audio

`daux_process_data` carries planar (non-interleaved) buffers as
`daux_audio_bus_buffer[]` for inputs and outputs, plus a `daux_process_context`
(sample rate, block size, timeline position, tempo, time signature, transport
flags). `float32` is the baseline; `float64` is reserved (`channels64`,
`DAUX_SAMPLE_F64`) and not yet honored by the reference core. MIDI fields are
present but `NULL` (reserved for a future revision).

## Parameters

- `uint32` id chosen by the plugin (stable across versions).
- `get_parameter_count()`, `get_parameter_info(index)` → `daux_param_info`
  (name/short/unit, min/max/default in **plain** units, step count, flags).
- `get_parameter_value(id)` / `set_parameter_value(id, value)` exchange **plain**
  values.
- `normalized_to_plain` / `plain_to_normalized` may be `NULL`, in which case the
  host assumes a linear map between min and max.
- `format_parameter` / `parse_parameter` are optional (may be `NULL`).

## State

`get_state(buf, size, &needed)` follows the size-then-fill convention: call with
`buf == NULL` to learn the required size (`DAUX_ERR_BUFFER_TOO_SMALL`,
`*needed` set), then again with a big-enough buffer. `set_state(data, size)`
restores. Both may be `NULL` if the plugin doesn't advertise `DAUX_CAP_STATE`.

## Editor

`create_editor(host, &editor)` yields a `daux_editor` (handle + vtable). The
editor vtable exposes `attach(parent)`, `detach`, `get_preferred_size`,
`set_bounds`, `show`, `hide`, and an optional `idle`. The editor is decoupled
from the audio vtable so plugins can run headless and so editors can be built by
different toolkits/languages and hosted (in-process or via DAUxHost) uniformly.
See [`dauxhost-protocol.md`](dauxhost-protocol.md).

## Result codes

`daux_result` — `0` success, negative error (see `daux_types.h`). Helper
`daux::result_to_string()` formats them.

## Design rules

1. Public ABI is stable C; never STL / Rust / .NET types.
2. Opaque handles, POD structs, function pointers only.
3. Headers split by concern: types / plugin / host / editor.
4. Explicit result-code enum.
5. Consistent `daux_` / `DAUx` prefixes.
6. Internal C++ (`daux_core`) kept separate from the public ABI.
7. No Futureboard-specific logic baked into the ABI.
8. Maps onto VST-like concepts (buses/params/state/editor) without copying any
   specific plugin API.
