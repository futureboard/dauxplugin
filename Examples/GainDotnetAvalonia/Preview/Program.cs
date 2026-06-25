// Program.cs - preview launcher for the Avalonia gain editor.
//
// Uses the example library's App (which, under a classic desktop lifetime,
// opens the GainEditorWindow bound to a fresh GainPlugin).
using Avalonia;
using Daux.Examples.Gain;

internal static class Program
{
    [STAThread]
    public static void Main(string[] args)
        => BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);

    public static AppBuilder BuildAvaloniaApp()
        => AppBuilder.Configure<App>()
            .UsePlatformDetect()
            .LogToTrace();
}
