# DAUx GUI Framework Layer

DAUx editors are **framework-agnostic**. The host never assumes Win32, Cocoa,
GPUI, Avalonia, Qt, or WebView — it only sees `DAUxGuiView` + `DAUxGuiViewInfo`.

## Framework enum

`DAUx/Gui/GuiFramework.h` lists known UI stacks (`DAUX_GUI_FRAMEWORK_GPUI`,
`DAUX_GUI_FRAMEWORK_AVALONIA`, …). Plugins report their framework via
`DAUxPluginClassInfo.gui_framework` and/or `DAUxController.get_gui_view_info`.

## Editor modes

| Mode | Use when |
|------|----------|
| **Embedded** (`DAUX_EDITOR_MODE_EMBEDDED`) | Host supplies a native parent (`HWND`, `NSView*`, X11 window). Good for native C++ UIs. |
| **External** (`DAUX_EDITOR_MODE_EXTERNAL`) | Plugin or DAUxHost owns a top-level window. Recommended first path for **GPUI**, **Avalonia**, Qt, WebView. |

Cross-runtime embedding (e.g. Avalonia HWND inside a DAW) is fragile; external
window mode avoids reparenting across runtimes.

## Message bridge

`DAUxGuiMessage` carries typed JSON/binary payloads between plugin UI and host
(resize, theme, DPI, parameter gestures). Callbacks are **non-realtime only**.

## Examples (current)

- **C++ gain** — `DAUX_GUI_FRAMEWORK_NATIVE`, embedded mode (no editor yet)
- **Rust/GPUI** — scaffold: external + GPUI (editor TODO)
- **.NET/Avalonia** — external + Avalonia (`manifest.xml` `<gui framework="avalonia" …>`)
