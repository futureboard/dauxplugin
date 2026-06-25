// Interop.cs - blittable native struct definitions and constants mirroring the
// DAUx Plugin C ABI (see daux/Core/include/daux_*.h).
//
// Every struct is StructLayout(Sequential) and field-for-field identical to the
// C headers. Function-pointer fields are stored as IntPtr and filled with
// `&UnmanagedCallersOnly` thunks at runtime (see DauxRuntime). Fixed-size char
// buffers use `fixed byte` so the structs stay blittable and can be written
// directly into unmanaged memory handed to the host.
//
// NOTE: keep these in lockstep with the C headers; bump nothing here without
// matching daux_types.h.
using System.Runtime.InteropServices;

namespace Daux.Plugin;

internal static class Abi
{
    public const uint AbiVersionMajor = 1;
    public const uint AbiVersionMinor = 0;
    public const uint AbiVersionPatch = 0;

    public const int IdSize = 64;
    public const int NameSize = 128;
    public const int ShortSize = 32;
    public const int VendorSize = 128;

    public const int Ok = 0;
    public const int ErrUnknown = -1;
    public const int ErrInvalidArg = -2;
    public const int ErrNotSupported = -3;
    public const int ErrNotInitialized = -4;
    public const int ErrOutOfMemory = -5;
    public const int ErrInvalidState = -6;
    public const int ErrBufferTooSmall = -7;
    public const int ErrAbiMismatch = -8;
    public const int ErrNotFound = -9;
    public const int ErrIo = -10;

    public const int CatUnknown = 0;
    public const int CatEffect = 1;
    public const int CatInstrument = 2;
    public const int CatMidiFx = 3;
    public const int CatAnalyzer = 4;

    public const int CapNone = 0;
    public const int CapEditor = 1 << 0;
    public const int CapMidiInput = 1 << 1;
    public const int CapMidiOutput = 1 << 2;
    public const int CapState = 1 << 3;
    public const int CapPresets = 1 << 4;

    public const int ParamNone = 0;
    public const int ParamAutomatable = 1 << 0;
    public const int ParamStepped = 1 << 1;
    public const int ParamReadOnly = 1 << 2;
    public const int ParamIsBypass = 1 << 3;

    public const int SampleF32 = 0;
    public const int SampleF64 = 1;

    public static uint MakeVersion(uint maj, uint min, uint pat)
        => ((maj & 0xFF) << 24) | ((min & 0xFF) << 16) | (pat & 0xFFFF);

    public static uint VersionMajor(uint v) => (v >> 24) & 0xFF;

    public static uint AbiVersion => MakeVersion(AbiVersionMajor, AbiVersionMinor, AbiVersionPatch);
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxProcessContext
{
    public double SampleRate;
    public uint BlockSize;
    public uint SampleFormat;
    public long TimelineSamples;
    public double TempoBpm;
    public int TimeSigNumerator;
    public int TimeSigDenominator;
    public int IsPlaying;
    public int IsRecording;
    public int IsLooping;
    // uint32 reserved[8]
    public uint R0, R1, R2, R3, R4, R5, R6, R7;
}

[StructLayout(LayoutKind.Sequential)]
internal unsafe struct DauxAudioBusBuffer
{
    public uint ChannelCount;
    public float** Channels;
    public double** Channels64;
}

[StructLayout(LayoutKind.Sequential)]
internal unsafe struct DauxProcessData
{
    public DauxProcessContext* Context;
    public uint InputBusCount;
    public DauxAudioBusBuffer* Inputs;
    public uint OutputBusCount;
    public DauxAudioBusBuffer* Outputs;
    public uint NumFrames;
    public void* MidiIn;
    public void* MidiOut;
    public uint R0, R1, R2, R3;
}

[StructLayout(LayoutKind.Sequential)]
internal unsafe struct DauxParamInfo
{
    public uint Id;
    public fixed byte Name[Abi.NameSize];
    public fixed byte ShortName[Abi.ShortSize];
    public fixed byte Unit[Abi.ShortSize];
    public double DefaultValue;
    public double MinValue;
    public double MaxValue;
    public int StepCount;
    public int Flags;
    public uint R0, R1, R2, R3;
}

[StructLayout(LayoutKind.Sequential)]
internal unsafe struct DauxPluginDescriptor
{
    public fixed byte Id[Abi.IdSize];
    public fixed byte Name[Abi.NameSize];
    public fixed byte Vendor[Abi.VendorSize];
    public uint Version;
    public int Category;
    public int Capabilities;
    public uint AudioInputBuses;
    public uint AudioOutputBuses;
    public uint DefaultChannelsPerBus;
    public uint R0, R1, R2, R3, R4, R5, R6, R7;
}

// Function-pointer-bearing structs use IntPtr fields filled at runtime.
[StructLayout(LayoutKind.Sequential)]
internal struct DauxHostCallbacks
{
    public uint StructSize;
    public uint AbiVersion;
    public IntPtr Context;
    public IntPtr ParamChanged;
    public IntPtr ParamGestureBegin;
    public IntPtr ParamGestureEnd;
    public IntPtr ResizeEditor;
    public IntPtr Log;
    public IntPtr R0, R1, R2, R3, R4, R5, R6, R7;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxPluginInstance
{
    public IntPtr Handle;
    public IntPtr Vtable;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxPluginVtable
{
    public uint StructSize;
    public IntPtr Prepare;
    public IntPtr Activate;
    public IntPtr Deactivate;
    public IntPtr Reset;
    public IntPtr Process;
    public IntPtr GetParameterCount;
    public IntPtr GetParameterInfo;
    public IntPtr GetParameterValue;
    public IntPtr SetParameterValue;
    public IntPtr NormalizedToPlain;
    public IntPtr PlainToNormalized;
    public IntPtr FormatParameter;
    public IntPtr ParseParameter;
    public IntPtr GetState;
    public IntPtr SetState;
    public IntPtr CreateEditor;
    public IntPtr DestroyEditor;
    public IntPtr R0, R1, R2, R3, R4, R5, R6, R7;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxSize
{
    public uint Width;
    public uint Height;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxRect
{
    public int X;
    public int Y;
    public uint Width;
    public uint Height;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxEditor
{
    public IntPtr Handle;
    public IntPtr Vtable;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxEditorVtable
{
    public uint StructSize;
    public IntPtr Attach;
    public IntPtr Detach;
    public IntPtr GetPreferredSize;
    public IntPtr SetBounds;
    public IntPtr Show;
    public IntPtr Hide;
    public IntPtr Idle;
    public IntPtr R0, R1, R2, R3, R4, R5;
}

[StructLayout(LayoutKind.Sequential)]
internal struct DauxPluginFactory
{
    public uint StructSize;
    public uint AbiVersion;
    public IntPtr GetDescriptor;
    public IntPtr Create;
    public IntPtr Destroy;
    public IntPtr R0, R1, R2, R3, R4, R5;
}
