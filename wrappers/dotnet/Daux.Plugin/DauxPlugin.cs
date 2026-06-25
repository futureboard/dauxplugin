// DauxPlugin.cs - the managed-facing API a C# plugin author works with.
//
// Authors implement IDauxPlugin (or derive from DauxPluginBase) and describe
// their plugin with PluginDescriptor / ParamInfo. None of these managed types
// cross the native boundary; DauxRuntime marshals them.
namespace Daux.Plugin;

public enum DauxCategory
{
    Effect = Abi.CatEffect,
    Instrument = Abi.CatInstrument,
    MidiFx = Abi.CatMidiFx,
    Analyzer = Abi.CatAnalyzer,
}

[Flags]
public enum DauxCaps
{
    None = Abi.CapNone,
    Editor = Abi.CapEditor,
    MidiInput = Abi.CapMidiInput,
    MidiOutput = Abi.CapMidiOutput,
    State = Abi.CapState,
    Presets = Abi.CapPresets,
}

[Flags]
public enum DauxParamFlags
{
    None = Abi.ParamNone,
    Automatable = Abi.ParamAutomatable,
    Stepped = Abi.ParamStepped,
    ReadOnly = Abi.ParamReadOnly,
    IsBypass = Abi.ParamIsBypass,
}

/// <summary>Static, instantiation-free plugin descriptor.</summary>
public sealed class PluginDescriptor
{
    public required string Id { get; init; }
    public required string Name { get; init; }
    public string Vendor { get; init; } = "";
    public (uint Major, uint Minor, uint Patch) Version { get; init; } = (0, 1, 0);
    public DauxCategory Category { get; init; } = DauxCategory.Effect;
    public DauxCaps Caps { get; init; } = DauxCaps.None;
    public uint AudioInputBuses { get; init; } = 1;
    public uint AudioOutputBuses { get; init; } = 1;
    public uint DefaultChannelsPerBus { get; init; } = 2;
}

/// <summary>Description of one parameter (values in plain units).</summary>
public sealed class ParamInfo
{
    public required uint Id { get; init; }
    public required string Name { get; init; }
    public string ShortName { get; init; } = "";
    public string Unit { get; init; } = "";
    public double Default { get; init; }
    public double Min { get; init; }
    public double Max { get; init; } = 1.0;
    public int StepCount { get; init; }
    public DauxParamFlags Flags { get; init; } = DauxParamFlags.Automatable;
}

/// <summary>Transport / timeline snapshot for one process block.</summary>
public readonly struct ProcessContext
{
    public double SampleRate { get; init; }
    public uint BlockSize { get; init; }
    public long TimelineSamples { get; init; }
    public double TempoBpm { get; init; }
    public (int Num, int Den) TimeSignature { get; init; }
    public bool IsPlaying { get; init; }
    public bool IsRecording { get; init; }
    public bool IsLooping { get; init; }
}

/// <summary>
/// Safe-ish accessor over the planar float32 process buffers. The slices it
/// returns are <see cref="Span{T}"/> over host-owned memory and are valid only
/// for the duration of the <see cref="IDauxPlugin.Process"/> call.
/// </summary>
public readonly unsafe ref struct AudioBuffers
{
    private readonly DauxProcessData* _data;

    internal AudioBuffers(DauxProcessData* data) => _data = data;

    public int Frames => (int)_data->NumFrames;
    public int InputBusCount => (int)_data->InputBusCount;
    public int OutputBusCount => (int)_data->OutputBusCount;

    public int InputChannels(int bus)
        => bus >= 0 && bus < InputBusCount ? (int)_data->Inputs[bus].ChannelCount : 0;
    public int OutputChannels(int bus)
        => bus >= 0 && bus < OutputBusCount ? (int)_data->Outputs[bus].ChannelCount : 0;

    public ReadOnlySpan<float> Input(int bus, int ch)
    {
        if (bus < 0 || bus >= InputBusCount) return default;
        ref readonly DauxAudioBusBuffer b = ref _data->Inputs[bus];
        if (b.Channels == null || ch < 0 || ch >= (int)b.ChannelCount) return default;
        float* p = b.Channels[ch];
        return p == null ? default : new ReadOnlySpan<float>(p, Frames);
    }

    public Span<float> Output(int bus, int ch)
    {
        if (bus < 0 || bus >= OutputBusCount) return default;
        ref readonly DauxAudioBusBuffer b = ref _data->Outputs[bus];
        if (b.Channels == null || ch < 0 || ch >= (int)b.ChannelCount) return default;
        float* p = b.Channels[ch];
        return p == null ? default : new Span<float>(p, Frames);
    }
}

/// <summary>Editor size in device-independent-ish pixels.</summary>
public readonly record struct EditorSize(uint Width, uint Height);

/// <summary>
/// A thin, managed view over the native <c>daux_host_callbacks</c> table. An
/// editor uses it to notify the host of parameter edits (so the host records
/// automation) and to request a resize. Lifetime: valid for as long as the
/// editor that received it.
/// </summary>
public sealed unsafe class HostBridge
{
    private readonly IntPtr _context;
    private readonly delegate* unmanaged[Cdecl]<IntPtr, uint, double, void> _paramChanged;
    private readonly delegate* unmanaged[Cdecl]<IntPtr, uint, void> _gestureBegin;
    private readonly delegate* unmanaged[Cdecl]<IntPtr, uint, void> _gestureEnd;
    private readonly delegate* unmanaged[Cdecl]<IntPtr, uint, uint, void> _resizeEditor;
    private readonly delegate* unmanaged[Cdecl]<IntPtr, int, byte*, void> _log;

    /// <summary>Creates a no-op bridge (all callbacks null). Useful for
    /// previews/tests where there is no real host.</summary>
    public HostBridge() { }

    internal HostBridge(DauxHostCallbacks* cb)
    {
        if (cb == null) return;
        _context = cb->Context;
        _paramChanged = (delegate* unmanaged[Cdecl]<IntPtr, uint, double, void>)cb->ParamChanged;
        _gestureBegin = (delegate* unmanaged[Cdecl]<IntPtr, uint, void>)cb->ParamGestureBegin;
        _gestureEnd = (delegate* unmanaged[Cdecl]<IntPtr, uint, void>)cb->ParamGestureEnd;
        _resizeEditor = (delegate* unmanaged[Cdecl]<IntPtr, uint, uint, void>)cb->ResizeEditor;
        _log = (delegate* unmanaged[Cdecl]<IntPtr, int, byte*, void>)cb->Log;
    }

    /// <summary>Send a log line to the host. level: 0=trace..4=error.</summary>
    public void Log(string message, int level = 2)
    {
        if (_log == null) return;
        byte[] bytes = System.Text.Encoding.UTF8.GetBytes(message + "\0");
        fixed (byte* p = bytes) _log(_context, level, p);
    }

    /// <summary>Report a parameter change (normalized [0,1]) to the host.</summary>
    public void ParamChanged(uint id, double normalized)
    {
        if (_paramChanged != null) _paramChanged(_context, id, normalized);
    }

    public void GestureBegin(uint id)
    {
        if (_gestureBegin != null) _gestureBegin(_context, id);
    }

    public void GestureEnd(uint id)
    {
        if (_gestureEnd != null) _gestureEnd(_context, id);
    }

    /// <summary>Ask the host to resize the editor window.</summary>
    public void RequestResize(uint width, uint height)
    {
        if (_resizeEditor != null) _resizeEditor(_context, width, height);
    }
}

/// <summary>
/// A plugin editor (GUI). Implemented by the toolkit-specific layer (e.g. the
/// Avalonia editor in the example). Mirrors <c>daux_editor_vtable</c>. All calls
/// arrive on the host UI thread.
/// </summary>
public interface IDauxEditor : IDisposable
{
    /// <summary>Attach the editor content into the host's native parent window
    /// (an HWND on Windows). Called before <see cref="Show"/>.</summary>
    void Attach(IntPtr parentNativeWindow);
    void Detach();
    EditorSize GetPreferredSize();
    void SetBounds(int x, int y, uint width, uint height);
    void Show();
    void Hide();
    /// <summary>Optional periodic tick from the host UI thread.</summary>
    void Idle();
}

/// <summary>The interface a C# plugin implements.</summary>
public interface IDauxPlugin
{
    void Prepare(double sampleRate, uint maxBlock);
    void Activate();
    void Deactivate();
    void Reset();

    void Process(in ProcessContext ctx, AudioBuffers buffers);

    double GetParameter(uint id);
    void SetParameter(uint id, double value);

    byte[] GetState();
    bool SetState(ReadOnlySpan<byte> data);

    /// <summary>Create the plugin's editor, or return null if it has none.
    /// <paramref name="host"/> lets the editor notify the host of edits.</summary>
    IDauxEditor? CreateEditor(HostBridge host);
}

/// <summary>Convenience base class with no-op defaults.</summary>
public abstract class DauxPluginBase : IDauxPlugin
{
    public virtual void Prepare(double sampleRate, uint maxBlock) { }
    public virtual void Activate() { }
    public virtual void Deactivate() { }
    public virtual void Reset() { }

    public abstract void Process(in ProcessContext ctx, AudioBuffers buffers);

    public abstract double GetParameter(uint id);
    public abstract void SetParameter(uint id, double value);

    public virtual byte[] GetState() => Array.Empty<byte>();
    public virtual bool SetState(ReadOnlySpan<byte> data) => true;

    /// <summary>No editor by default. Override to return one and set
    /// <see cref="DauxCaps.Editor"/> in the descriptor.</summary>
    public virtual IDauxEditor? CreateEditor(HostBridge host) => null;
}
