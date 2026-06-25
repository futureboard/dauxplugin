// AvaloniaRuntime.cs - owns a dedicated Avalonia UI thread for plugin-hosted
// editors.
//
// When the editor is created by the host (not by the preview app), there is no
// Avalonia application or dispatcher running. We start one lazily on a private
// STA background thread and pump its dispatcher with MainLoop, so editor
// windows can be created/shown on demand. All UI work is marshalled onto this
// thread via Post/Invoke.
using Avalonia;
using Avalonia.Threading;

namespace Daux.Examples.Gain;

internal static class AvaloniaRuntime
{
    private static Thread? _uiThread;
    private static volatile bool _started;
    private static readonly object _gate = new();

    public static void EnsureStarted()
    {
        if (_started) return;
        lock (_gate)
        {
            if (_started) return;

            using var ready = new ManualResetEventSlim(false);
            _uiThread = new Thread(() =>
            {
                // Configure Avalonia without a lifetime; we drive the loop ourselves.
                AppBuilder.Configure<App>()
                    .UsePlatformDetect()
                    .SetupWithoutStarting();

                ready.Set();
                // Pump the dispatcher until the process exits.
                Dispatcher.UIThread.MainLoop(CancellationToken.None);
            })
            {
                IsBackground = true,
                Name = "DAUx-Avalonia-UI",
            };
            if (OperatingSystem.IsWindows())
                _uiThread.SetApartmentState(ApartmentState.STA);
            _uiThread.Start();
            ready.Wait();
            _started = true;
        }
    }

    /// <summary>Queue work on the UI thread (fire-and-forget).</summary>
    public static void Post(Action action)
    {
        EnsureStarted();
        Dispatcher.UIThread.Post(action);
    }

    /// <summary>Run work on the UI thread and wait for it to complete.</summary>
    public static void Invoke(Action action)
    {
        EnsureStarted();
        Dispatcher.UIThread.InvokeAsync(action).GetTask().Wait();
    }
}
