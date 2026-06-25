# Avalonia/DAUx editor

The Avalonia editor for the C# gain example is **implemented** (no longer a
placeholder). Code lives in the `Daux.Examples.Gain` project:

| File | Role |
|---|---|
| `App.axaml` / `App.axaml.cs` | Avalonia application; opens the editor window under a classic desktop lifetime (preview). |
| `GainEditorWindow.axaml` / `.cs` | The editor UI: a gain slider + dB readout wired to the plugin parameter. |
| `GainEditor.cs` | `IDauxEditor` implementation mapping `attach`/`show`/`hide`/`set_bounds`/`idle` onto the window. |
| `AvaloniaRuntime.cs` | Dedicated Avalonia UI thread for plugin-hosted editors. |
| `NativeEmbed.cs` | Win32 `SetParent` for reparenting the editor into the host window. |

## Run it

```powershell
cd ..\Preview
dotnet run -c Release      # opens the editor window standalone
```

Through the C ABI (creates the editor, reports preferred size):

```powershell
DAUxHost.exe --mode=editor --plugin=...\daux_gain_dotnet.dauxplug
```

## How it fits together

- The plugin advertises `DauxCaps.Editor` and returns a `GainEditor` from
  `CreateEditor(HostBridge)`.
- `Daux.Plugin.DauxRuntime` exposes the editor across the C ABI via the
  `daux_editor_vtable` (create/destroy + attach/detach/size/bounds/show/hide/idle).
- Slider edits update the plugin immediately and call `HostBridge.ParamChanged`
  so the host can record automation.

## Remaining TODO

- **Full window embedding**: `NativeEmbed.SetParent` reparents the HWND, but
  child-window style flags (clear `WS_POPUP`/caption), sizing to the parent
  client rect, and focus handling are not done yet.
- **Out-of-process hosting**: `DAUxHost --mode=editor` currently creates +
  probes the editor; it does not yet host a top-level window and pump a message
  loop / reparent across processes. See `daux/docs/dauxhost-protocol.md`.
- **Native render deps when shown out-of-process**: showing the editor (vs.
  probing) loads SkiaSharp/HarfBuzz native libs, which must sit next to the
  plugin. The preview app bundles them automatically.
