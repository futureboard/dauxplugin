using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using Daux.Plugin;

namespace Daux.Examples.Gain;

/// <summary>
/// The Avalonia application object. Dual-purpose:
///   * Under a classic desktop lifetime (the preview app) it opens a default
///     editor window so the GUI can be run with `dotnet run`.
///   * When set up without a lifetime (AvaloniaRuntime, for plugin hosting) it
///     stays headless and editor windows are created on demand.
/// </summary>
public sealed class App : Application
{
    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            // Preview: bind the editor to a fresh plugin instance.
            desktop.MainWindow = new GainEditorWindow(new GainPlugin(), new HostBridge());
        }
        base.OnFrameworkInitializationCompleted();
    }
}
