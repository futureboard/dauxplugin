# Graphics architecture

Most plug-in SDKs have one knob called "the graphics backend". DAUxPlug has three, because
in practice three things vary independently and collapsing them is what makes plug-in UI
code impossible to port later.

```
        UI framework            Rendering backend         Presentation mode
        ────────────            ─────────────────         ─────────────────
        egui                    wgpu                      NativeWindow
        GPUI                    OpenGL                    EmbeddedSurface
        custom                  software                  SharedTexture
                                                          ExternalWindow
```

An egui editor rendered by wgpu into an embedded child window, an egui editor rendered by
software into a host-owned surface, and a GPUI editor handed to the host as a shared GPU
texture are three points in this space — not three "backends". `daux-graphics` models the
space; the backend crates each occupy part of it.

## Layering

```
                 your editor code
                        │
        ┌───────────────▼────────────────┐
        │  daux-graphics                 │  no GUI framework, no GPU API,
        │  DauxGraphic · GraphicContext  │  no platform code. raw-window-handle only.
        │  InputEvent · ParamBinding     │
        └───┬─────────────┬──────────┬───┘
            │             │          │
   ┌────────▼──┐  ┌───────▼──────┐  ┌▼───────────────┐
   │ -egui     │  │ -gpui        │  │ your own impl  │   frameworks
   └────────┬──┘  └───────┬──────┘  └┬───────────────┘
            │             │          │
   ┌────────▼─────────────▼──────────▼───┐
   │  -wgpu   ·   -gl   ·   software     │              renderers
   └──────────────────┬──────────────────┘
                      │
   ┌──────────────────▼──────────────────┐
   │  HWND · NSView · X11 · Wayland      │              presentation
   │  or a shared GPU texture            │
   └─────────────────────────────────────┘
```

`daux-graphics` depends on `raw-window-handle` and nothing else. A headless plug-in that
never draws anything compiles without a single GPU or UI crate in its dependency tree —
which is why the backend crates are optional features and are excluded from
`default-members`.

## The editor is not the plug-in

An editor may be created and destroyed many times during one plug-in instance's life, and a
plug-in instance is perfectly usable with no editor at all. Therefore:

- The editor never owns DSP state.
- The editor never owns the parameter set — it borrows the same `Arc<Params>` the processor
  reads.
- Queues between the audio thread and the UI are owned by the **plug-in instance**, and the
  editor borrows the reader end. When the editor closes, the audio thread keeps writing
  into a queue nobody is draining, which is exactly why those queues are bounded and
  overwrite-oldest rather than growable.
- Closing an editor must not change what the plug-in outputs. If it does, the plug-in has a
  bug that will show up as "my automation sounds different when the window is open".

## Presentation modes

**NativeWindow** — the plug-in creates its own top-level window. Simple, correct, and the
only option in some hosts. Used for standalone previews (`daux run --editor`).

**EmbeddedSurface** — the host passes a parent (`HWND`, `NSView`, X11 `Window`, Wayland
surface) and the plug-in parents its content into it. This is what VST3 and CLAP hosts do,
and it is the universal fallback that every plug-in must support.

**SharedTexture** — the plug-in renders into a GPU resource the host imports and composites
into its own scene. No nested child window, no separate swapchain, no z-order fights, and
the host can apply its own transforms, animations and DPI handling to the plug-in's pixels.
This is the DAUx-native capability that VST3 and CLAP cannot express.

**ExternalWindow** — the editor lives in another process (sandboxed hosting) and the host
is told where it is. The mode exists so the sandbox path is not a special case bolted on
later.

### Shared textures, honestly

The handle kinds are platform-specific and interop is fiddly:

| Platform | Path                                            |
| -------- | ----------------------------------------------- |
| Windows  | D3D11 `IDXGIResource1` shared HANDLE, D3D12 heap |
| macOS    | `IOSurface` backing a Metal texture              |
| Linux    | DMA-BUF, or Vulkan external memory FDs           |

Three rules keep this from becoming a source of crashes:

1. **Negotiation is mandatory.** The host advertises the handle kinds and formats it can
   import; the plug-in picks one or declines. No implicit assumptions.
2. **Fallback is mandatory.** A plug-in must never *require* shared textures to show a UI.
   If negotiation fails, it falls back to `EmbeddedSurface`.
3. **Synchronisation is explicit.** A shared texture without a fence is a race. The
   `SharedTexture` struct carries an optional fence handle, and a presenter that cannot
   provide one must say so during negotiation so the host can fall back.

`daux-graphics` defines the negotiation and the types. It does not call a single GPU API —
that lives in the backend crates, where it can be tested against a real device.

## Input

Editors need more than "mouse moved". `InputEvent` covers pointer motion, buttons with
modifiers, pixel and line scroll, key press/release with platform-neutral codes, text
input, focus changes, IME preedit and commit, and drag-and-drop. Every handler returns an
`InputResponse` so the plug-in can tell the host whether it consumed the event — hosts need
that to decide whether a keystroke was a plug-in shortcut or a transport command.

Coordinates are **logical** (scale-independent) at the API surface, with the scale factor
delivered separately. Physical and logical sizes are distinct types, so a HiDPI bug cannot
compile.

## Parameters in the UI

Every backend needs the same four things for a knob: read the value, show it as text, drive
a host automation gesture, and parse typed input. `ParamBinding` implements all four once,
in `daux-graphics`, so backends don't each reinvent the gesture state machine:

```rust
let b = ParamBinding::new(&params.gain, host.params());
b.begin_gesture();              // on drag start — idempotent
b.set_normalized(new_value);    // during drag
b.end_gesture();                // on release — safe even if begin never happened
```

Getting this wrong is the single most common plug-in bug: gestures that never end leave the
host's automation lane latched in write mode. The binding tracks its own state and refuses
to emit an unbalanced pair.

## Rendering off the audio thread — and off the UI thread's critical path

Meters, spectra and waveform overviews are produced on the audio thread and consumed at
frame rate. They travel through a `TripleBuffer`: the audio thread writes the latest
snapshot without ever waiting, the UI reads whatever is newest, and dropped intermediate
frames are simply not a problem for a 60 Hz display. Nothing about drawing ever blocks the
audio thread, and nothing about the audio thread ever blocks drawing.
