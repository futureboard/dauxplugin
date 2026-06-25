# DAUx Plugin SDK — C# / .NET

[`Daux.Plugin`](../wrappers/dotnet/Daux.Plugin) is the managed SDK. You
implement `IDauxPlugin` (or derive from `DauxPluginBase`) and add one tiny
native entry point; the SDK marshals everything across the C ABI.

## Why NativeAOT (architecture decision)

A DAUx plugin must export the C symbol `daux_plugin_entry` from a native
dynamic library. The CLR cannot export C symbols from an ordinary managed
assembly. The supported, **no-extra-shim** way to do this is **.NET NativeAOT**:

- it compiles managed code ahead-of-time to a native library, and
- it exports `static` methods annotated `[UnmanagedCallersOnly(EntryPoint=...)]`.

So the plugin **boundary stays pure C ABI** (function pointers + blittable POD
structs) and no managed object ever crosses it. The alternative — a native C
shim DLL that hosts the CLR and forwards calls — was rejected: it adds a second
artifact, complicates deployment, and `UnmanagedCallersOnly` + NativeAOT already
gives a single self-contained `.dll`.

Consequences / constraints this imposes on the SDK:

- All ABI thunks are `static` and **non-generic** (NativeAOT requirement), so
  instance dispatch goes through a `GCHandle`: the native "handle" is
  `GCHandle.ToIntPtr(GCHandle.Alloc(plugin))`.
- The factory/vtable/descriptor are written into native memory
  (`NativeMemory.AllocZeroed`) so the host gets stable pointers.

## Writing a plugin

```csharp
using Daux.Plugin;

public sealed class GainPlugin : DauxPluginBase
{
    private double _db;
    public static PluginDescriptor Descriptor => new()
    {
        Id = "com.example.gain", Name = "Gain", Vendor = "Me",
        Caps = DauxCaps.State,
    };
    public static ParamInfo[] Parameters => new[]
    {
        new ParamInfo { Id = 1, Name = "Gain", Unit = "dB", Min = -60, Max = 12 },
    };
    public override void Process(in ProcessContext ctx, AudioBuffers b)
    {
        float g = (float)Math.Pow(10, _db / 20);
        for (int bus = 0; bus < b.OutputBusCount; bus++)
        for (int ch = 0; ch < b.OutputChannels(bus); ch++)
        {
            var o = b.Output(bus, ch); var i = b.Input(bus, ch);
            for (int n = 0; n < o.Length; n++)
                o[n] = (n < i.Length ? i[n] : 0f) * g;
        }
    }
    public override double GetParameter(uint id) => _db;
    public override void SetParameter(uint id, double v) => _db = Math.Clamp(v, -60, 12);
}
```

The single native export:

```csharp
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using Daux.Plugin;

public static class Entry
{
    [UnmanagedCallersOnly(EntryPoint = "daux_plugin_entry",
                          CallConvs = new[] { typeof(CallConvCdecl) })]
    public static IntPtr DauxPluginEntry(uint hostAbiVersion)
        => DauxRuntime.GetEntry(hostAbiVersion,
                                GainPlugin.Descriptor,
                                GainPlugin.Parameters,
                                static () => new GainPlugin());
}
```

`.csproj` essentials:

```xml
<OutputType>Library</OutputType>
<PublishAot>true</PublishAot>
<NativeLib>Shared</NativeLib>
<AllowUnsafeBlocks>true</AllowUnsafeBlocks>
<RuntimeIdentifier>win-x64</RuntimeIdentifier>
```

## Building

- Development / IntelliSense: `dotnet build` (managed assembly only).
- Ship a plugin: `dotnet publish -r win-x64 -c Release`
  → `bin/Release/net8.0/win-x64/native/<name>.dll`. Rename/copy to
  `<name>.dauxplug`.

> NativeAOT publish needs the C++ build tools and `vswhere.exe` reachable. If
> publish fails with `'vswhere.exe' is not recognized`, run from a *Developer
> PowerShell for VS* or prepend the VS Installer dir to `PATH`:
> `"$env:ProgramFiles(x86)\Microsoft Visual Studio\Installer"`.

## Editor (Avalonia) — implemented

The SDK exposes a toolkit-agnostic editor seam:

- `IDauxEditor` (attach/detach/get-preferred-size/set-bounds/show/hide/idle)
  mirrors `daux_editor_vtable`.
- `IDauxPlugin.CreateEditor(HostBridge)` returns one; set `DauxCaps.Editor`.
- `HostBridge` wraps `daux_host_callbacks` so the editor can report parameter
  edits (`ParamChanged`) and request resizes.
- `DauxRuntime` builds the native editor vtable and routes `create_editor` /
  `destroy_editor` + all editor calls through `GCHandle`-pinned instances.

The example (`gain-dotnet-avalonia`) implements an Avalonia editor:

- `Daux.Examples.Gain` (library): plugin + Avalonia `GainEditorWindow` +
  `GainEditor : IDauxEditor` + `AvaloniaRuntime` (dedicated UI thread).
- `Plugin` (cdylib): the NativeAOT `.dauxplug` shell (just `Entry.cs`).
- `Preview` (exe): `dotnet run` to open the editor standalone.

Verify through the ABI: `DAUxHost --mode=editor --plugin=<.dauxplug>` creates
the editor and prints its preferred size.

## Remaining TODO

- Full HWND child-window embedding (style flags, sizing, focus); see
  `examples/gain-dotnet-avalonia/Editor/README.md`.
- Out-of-process window hosting + message loop in `DAUxHost --mode=editor`.
- Normalized↔plain curves and parameter formatting.
