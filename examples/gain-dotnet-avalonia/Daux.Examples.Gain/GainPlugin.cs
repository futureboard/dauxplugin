// GainPlugin.cs - the C# example plugin (DSP + state + editor factory).
//
// Mirrors the C++/Rust gain examples but additionally advertises an editor
// (DauxCaps.Editor) and returns an Avalonia-based GainEditor from CreateEditor.
using Daux.Plugin;

namespace Daux.Examples.Gain;

public sealed class GainPlugin : DauxPluginBase
{
    public const uint ParamGain = 1;
    public const double GainMinDb = -60.0;
    public const double GainMaxDb = 12.0;
    public const double GainDefaultDb = 0.0;
    private const uint StateMagic = 0x4744_4144; // "DADG"

    private double _gainDb = GainDefaultDb;

    /// <summary>Current gain in dB. Read/written by the editor and the host.</summary>
    public double GainDb
    {
        get => _gainDb;
        set => _gainDb = Math.Clamp(value, GainMinDb, GainMaxDb);
    }

    public static PluginDescriptor Descriptor => new()
    {
        Id = "com.daux.examples.gain.dotnet",
        Name = "DAUx Gain (.NET)",
        Vendor = "DAUx",
        Version = (0, 1, 0),
        Category = DauxCategory.Effect,
        Caps = DauxCaps.State | DauxCaps.Editor,
        AudioInputBuses = 1,
        AudioOutputBuses = 1,
        DefaultChannelsPerBus = 2,
    };

    public static ParamInfo[] Parameters => new[]
    {
        new ParamInfo
        {
            Id = ParamGain,
            Name = "Gain",
            Unit = "dB",
            Min = GainMinDb,
            Max = GainMaxDb,
            Default = GainDefaultDb,
            Flags = DauxParamFlags.Automatable,
        },
    };

    private static float DbToLinear(double db) => (float)Math.Pow(10.0, db / 20.0);

    public override void Process(in ProcessContext ctx, AudioBuffers buffers)
    {
        float g = DbToLinear(_gainDb);
        int frames = buffers.Frames;
        for (int bus = 0; bus < buffers.OutputBusCount; bus++)
        {
            int channels = buffers.OutputChannels(bus);
            for (int ch = 0; ch < channels; ch++)
            {
                Span<float> outBuf = buffers.Output(bus, ch);
                if (outBuf.IsEmpty) continue;
                ReadOnlySpan<float> inBuf = buffers.Input(bus, ch);
                if (!inBuf.IsEmpty)
                {
                    int n = Math.Min(frames, Math.Min(outBuf.Length, inBuf.Length));
                    for (int i = 0; i < n; i++) outBuf[i] = inBuf[i] * g;
                }
                else
                {
                    outBuf.Clear();
                }
            }
        }
    }

    public override double GetParameter(uint id) => id == ParamGain ? _gainDb : 0.0;

    public override void SetParameter(uint id, double value)
    {
        if (id == ParamGain) GainDb = value;
    }

    public override byte[] GetState()
    {
        var buf = new byte[12];
        BitConverter.TryWriteBytes(buf.AsSpan(0, 4), StateMagic);
        BitConverter.TryWriteBytes(buf.AsSpan(4, 8), _gainDb);
        return buf;
    }

    public override bool SetState(ReadOnlySpan<byte> data)
    {
        if (data.Length < 12) return false;
        if (BitConverter.ToUInt32(data.Slice(0, 4)) != StateMagic) return false;
        GainDb = BitConverter.ToDouble(data.Slice(4, 8));
        return true;
    }

    public override IDauxEditor? CreateEditor(HostBridge host) => new GainEditor(this, host);

    // --- helpers shared with the editor ------------------------------------
    public double NormalizedGain => (_gainDb - GainMinDb) / (GainMaxDb - GainMinDb);
}
