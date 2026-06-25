using System.Globalization;
using Avalonia.Controls;
using Avalonia.Markup.Xaml;
using Daux.Plugin;

namespace Daux.Examples.Gain;

/// <summary>
/// The editor's Avalonia window: a single gain slider wired directly to the
/// plugin's parameter. Edits update the plugin immediately and notify the host
/// (so it can record automation) via the <see cref="HostBridge"/>.
/// </summary>
public partial class GainEditorWindow : Window
{
    private readonly GainPlugin _plugin;
    private readonly HostBridge _host;
    private readonly Slider _slider;
    private readonly TextBlock _value;
    private bool _suppress;

    // Parameterless ctor only for the XAML designer / loader; not used at runtime.
    public GainEditorWindow() : this(new GainPlugin(), new HostBridge()) { }

    public GainEditorWindow(GainPlugin plugin, HostBridge host)
    {
        _plugin = plugin;
        _host = host;
        AvaloniaXamlLoader.Load(this);

        _slider = this.FindControl<Slider>("GainSlider")!;
        _value = this.FindControl<TextBlock>("GainValue")!;

        _suppress = true;
        _slider.Value = _plugin.GainDb;
        _suppress = false;
        UpdateLabel(_plugin.GainDb);

        // Tell the host once the window is actually up (Skia surface created).
        // Proves to a non-.NET host that rendering initialized successfully.
        Opened += (_, _) => _host.Log("editor window opened (Avalonia/Skia rendering active)");

        _slider.PropertyChanged += (_, e) =>
        {
            if (e.Property != RangeBase_ValueProperty()) return;
            if (_suppress) return;
            double db = _slider.Value;
            _plugin.GainDb = db;
            UpdateLabel(db);
            // Notify the host with the normalized value (host records automation).
            _host.ParamChanged(GainPlugin.ParamGain, _plugin.NormalizedGain);
        };
    }

    // Avalonia's Slider.Value lives on RangeBase; resolve the property once.
    private static Avalonia.AvaloniaProperty RangeBase_ValueProperty()
        => Avalonia.Controls.Primitives.RangeBase.ValueProperty;

    private void UpdateLabel(double db)
        => _value.Text = db.ToString("0.0", CultureInfo.InvariantCulture) + " dB";

    /// <summary>Push an external parameter change (e.g. host automation) into
    /// the UI without re-notifying the host.</summary>
    public void SyncFromPlugin()
    {
        _suppress = true;
        _slider.Value = _plugin.GainDb;
        _suppress = false;
        UpdateLabel(_plugin.GainDb);
    }
}
