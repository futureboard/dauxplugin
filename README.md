# DAUx Plugin Platform

A native plugin platform for **Futureboard** (and future external use), built
around a stable **C ABI** with language wrappers and an out-of-process host.

Three layers:

1. **DAUx Plugin Core** — the C ABI (`daux/core/include`) + a C++ host-side
   helper library (`daux_core`).
2. **Wrapper SDKs** — write plugins in **Rust** (`daux-plugin`) or **C#/.NET**
   (`Daux.Plugin`); both call the same C ABI.
3. **DAUxHost.exe** — loads/runs `.dauxplug` out-of-process, hosts editors,
   scans metadata, and (planned) bridges to the DAW over IPC.

```
daux/
├─ core/            # C ABI headers + daux_core C++ helper lib
├─ wrappers/
│  ├─ rust/         # daux-plugin-sys (raw FFI) + daux-plugin (safe)
│  └─ dotnet/       # Daux.Plugin (NativeAOT bridge)
├─ host/
│  ├─ dauxhost/     # DAUxHost.exe (load/scan/headless; editor+IPC skeleton)
│  └─ dauxscan/     # DAUxScan.exe (standalone scanner)
├─ examples/
│  ├─ gain-cpp-headless/    # reference C++ plugin
│  ├─ gain-rust-gpui/       # Rust plugin (+ GPUI editor scaffold)
│  └─ gain-dotnet-avalonia/ # C# plugin + working Avalonia editor + preview
└─ docs/            # ABI spec, SDK guides, host protocol
```

Status: the audio path works end-to-end in all three languages. The C#/Avalonia
example has a **working editor** (runnable via the preview app and reachable
through the C ABI / `DAUxHost --mode=editor`); host-side window hosting and IPC
are scaffolded with clear TODOs. See [`docs/`](docs).

## Build & run

### One command (recommended)

Build everything (Core + host + all three example plugins) and verify with a
scan:

```powershell
daux\scripts\build.cmd            # Windows  (all | core | rust | dotnet)
```
```bash
chmod +x daux/scripts/build.sh
daux/scripts/build.sh             # Linux / macOS  (all | core | rust | dotnet)
```

The scripts pick the right runtime identifier and library extension per OS,
assemble the .NET Avalonia plugin as a bundle, and drop all plugins in
`build/plugins`. The sections below show the equivalent manual steps.

### Core + host + C++ example (CMake)

```powershell
cmake -S daux -B build -A x64
cmake --build build --config Release
```

Produces `build/bin/Release/DAUxHost.exe`, `DAUxScan.exe`, and
`build/plugins/Release/daux_gain_cpp.dauxplug`.

```powershell
build\bin\Release\DAUxHost.exe --mode=headless --plugin=build\plugins\Release\daux_gain_cpp.dauxplug
```

### Rust example (cargo)

```powershell
cd daux\examples\gain-rust-gpui
cargo build --release
Copy-Item target\release\gain_rust_gpui.dll ..\..\..\build\plugins\daux_gain_rust.dauxplug
```

### .NET example + Avalonia editor (NativeAOT)

The example is three projects: `Daux.Examples.Gain` (plugin DSP + Avalonia
editor), `Plugin` (the NativeAOT `.dauxplug` shell), and `Preview` (a runnable
GUI harness).

Preview the editor GUI without a DAW:

```powershell
cd daux\examples\gain-dotnet-avalonia\Preview
dotnet run -c Release
```

Build the shippable plugin as a **bundle** (`.dauxplug` folder with
`Exec/` + `Library/` + `Resources/` + `manifest.xml` — see
[Packaging](docs/plugin-abi.md#packaging-single-file-vs-bundle)):

```powershell
cd daux\examples\gain-dotnet-avalonia
dotnet publish Plugin\gain-dotnet-avalonia.csproj -r win-x64 -c Release
$pub = "Plugin\bin\Release\net8.0\win-x64\publish"
$bundle = "..\..\..\build\plugins\daux_gain_dotnet.dauxplug"
New-Item -ItemType Directory -Force "$bundle\Exec","$bundle\Library","$bundle\Resources" | Out-Null
Copy-Item "$pub\gain-dotnet-avalonia.dll" "$bundle\Exec\daux_gain_dotnet.dll"
Copy-Item "$pub\libSkiaSharp.dll","$pub\libHarfBuzzSharp.dll","$pub\av_libglesv2.dll" "$bundle\Library"
Copy-Item manifest.xml "$bundle\manifest.xml"
```

(C++ and Rust plugins are single-file `.dauxplug` — no bundle needed.)

(If publish reports `'vswhere.exe' is not recognized`, run from a *Developer
PowerShell for VS* — see [docs/plugin-sdk-dotnet.md](docs/plugin-sdk-dotnet.md).)

The editor is reachable through the C ABI; verify cross-language editor
creation with:

```powershell
build\bin\Release\DAUxHost.exe --mode=editor --plugin=build\plugins\daux_gain_dotnet.dauxplug
```

### Verify all three

```powershell
build\bin\Release\DAUxScan.exe build\plugins
```

Expected: three `[OK]` lines (C++, Rust, .NET), 0 failures — all loaded through
the same C ABI by the same host.

## Documentation

- [Plugin ABI](docs/plugin-abi.md)
- [Rust SDK](docs/plugin-sdk-rust.md)
- [.NET SDK](docs/plugin-sdk-dotnet.md)
- [DAUxHost protocol](docs/dauxhost-protocol.md)
