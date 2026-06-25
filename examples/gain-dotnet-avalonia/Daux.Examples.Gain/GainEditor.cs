// GainEditor.cs - IDauxEditor implementation backed by an Avalonia window.
//
// Maps the DAUx editor lifecycle (attach/detach/show/hide/set_bounds/idle) onto
// an Avalonia GainEditorWindow running on the shared AvaloniaRuntime UI thread.
// When the host provides a parent HWND, the window is reparented into it
// (best-effort; see NativeEmbed).
using Avalonia;
using Daux.Plugin;

namespace Daux.Examples.Gain;

public sealed class GainEditor : IDauxEditor
{
    private readonly GainPlugin _plugin;
    private readonly HostBridge _host;
    private IntPtr _parent;
    private GainEditorWindow? _window;
    private bool _shown;

    public GainEditor(GainPlugin plugin, HostBridge host)
    {
        _plugin = plugin;
        _host = host;
    }

    public void Attach(IntPtr parentNativeWindow) => _parent = parentNativeWindow;

    public EditorSize GetPreferredSize() => new(360, 200);

    public void SetBounds(int x, int y, uint width, uint height)
    {
        AvaloniaRuntime.Post(() =>
        {
            if (_window == null) return;
            _window.Position = new PixelPoint(x, y);
            _window.Width = width;
            _window.Height = height;
        });
    }

    public void Show()
    {
        _shown = true;
        AvaloniaRuntime.Post(() =>
        {
            _window ??= new GainEditorWindow(_plugin, _host);
            _window.Show();
            TryEmbed();
        });
    }

    public void Hide()
    {
        if (!_shown) return;
        AvaloniaRuntime.Post(() => _window?.Hide());
    }

    public void Detach()
    {
        // Nothing to tear down if the window was never shown - avoid spinning
        // up the Avalonia UI thread just to dispose.
        if (!_shown) return;
        AvaloniaRuntime.Post(() =>
        {
            _window?.Close();
            _window = null;
        });
    }

    public void Idle() { /* meters/animation tick - nothing to do for gain */ }

    public void Dispose() => Detach();

    private void TryEmbed()
    {
        if (_parent == IntPtr.Zero || _window == null) return;
        var handle = _window.TryGetPlatformHandle();
        if (handle == null) return;
        // Reparent the editor's HWND into the host-provided parent. Best-effort;
        // full child-window styling/sizing is a TODO.
        NativeEmbed.SetParent(handle.Handle, _parent);
    }
}
