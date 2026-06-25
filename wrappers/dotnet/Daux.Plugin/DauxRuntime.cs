// DauxRuntime.cs - the bridge from managed IDauxPlugin to the native C ABI.
//
// A C# plugin's NativeAOT entry point delegates to DauxRuntime.GetEntry, which
// lazily builds the unmanaged factory/vtable/descriptor (in native memory so
// the pointers are stable) and returns the factory pointer to the host.
//
// Instance model: each plugin instance is a managed IDauxPlugin pinned by a
// GCHandle; the native "handle" is GCHandle.ToIntPtr. Every vtable thunk is a
// static [UnmanagedCallersOnly] method (NativeAOT requires non-generic, static)
// that recovers the instance from the handle and calls the interface.
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace Daux.Plugin;

public static unsafe class DauxRuntime
{
    private static PluginDescriptor? _descriptor;
    private static ParamInfo[] _params = Array.Empty<ParamInfo>();
    private static Func<IDauxPlugin>? _activator;

    private static IntPtr _descriptorPtr;
    private static IntPtr _vtablePtr;
    private static IntPtr _editorVtablePtr;
    private static IntPtr _factoryPtr;
    private static bool _initialized;

    /// <summary>
    /// Implements the body of the exported <c>daux_plugin_entry</c>. The plugin
    /// supplies its descriptor, parameter table and an activator delegate. Call
    /// from the plugin's [UnmanagedCallersOnly(EntryPoint="daux_plugin_entry")].
    /// </summary>
    public static IntPtr GetEntry(
        uint hostAbiVersion,
        PluginDescriptor descriptor,
        IReadOnlyList<ParamInfo> parameters,
        Func<IDauxPlugin> activator)
    {
        if (Abi.VersionMajor(hostAbiVersion) != Abi.AbiVersionMajor)
            return IntPtr.Zero; // refuse mismatched host

        EnsureInitialized(descriptor, parameters, activator);
        return _factoryPtr;
    }

    private static void EnsureInitialized(
        PluginDescriptor descriptor, IReadOnlyList<ParamInfo> parameters, Func<IDauxPlugin> activator)
    {
        if (_initialized) return;

        _descriptor = descriptor;
        _params = parameters.ToArray();
        _activator = activator;

        _descriptorPtr = BuildDescriptor(descriptor);
        _editorVtablePtr = BuildEditorVtable();
        _vtablePtr = BuildVtable();
        _factoryPtr = BuildFactory();
        _initialized = true;
    }

    // --- native struct builders (allocate stable unmanaged memory) ----------
    private static IntPtr BuildDescriptor(PluginDescriptor d)
    {
        var ptr = (DauxPluginDescriptor*)NativeMemory.AllocZeroed((nuint)sizeof(DauxPluginDescriptor));
        WriteUtf8(ptr->Id, Abi.IdSize, d.Id);
        WriteUtf8(ptr->Name, Abi.NameSize, d.Name);
        WriteUtf8(ptr->Vendor, Abi.VendorSize, d.Vendor);
        ptr->Version = Abi.MakeVersion(d.Version.Major, d.Version.Minor, d.Version.Patch);
        ptr->Category = (int)d.Category;
        ptr->Capabilities = (int)d.Caps;
        ptr->AudioInputBuses = d.AudioInputBuses;
        ptr->AudioOutputBuses = d.AudioOutputBuses;
        ptr->DefaultChannelsPerBus = d.DefaultChannelsPerBus;
        return (IntPtr)ptr;
    }

    private static IntPtr BuildVtable()
    {
        var vt = (DauxPluginVtable*)NativeMemory.AllocZeroed((nuint)sizeof(DauxPluginVtable));
        vt->StructSize = (uint)sizeof(DauxPluginVtable);
        vt->Prepare = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, double, uint, int>)&Prepare;
        vt->Activate = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&Activate;
        vt->Deactivate = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&Deactivate;
        vt->Reset = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&Reset;
        vt->Process = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, DauxProcessData*, int>)&Process;
        vt->GetParameterCount = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, uint>)&GetParameterCount;
        vt->GetParameterInfo = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, uint, DauxParamInfo*, int>)&GetParameterInfo;
        vt->GetParameterValue = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, uint, double*, int>)&GetParameterValue;
        vt->SetParameterValue = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, uint, double, int>)&SetParameterValue;
        // Linear params: leave conversions / formatting null (host default).
        vt->GetState = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, void*, uint, uint*, int>)&GetState;
        vt->SetState = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, void*, uint, int>)&SetState;
        vt->CreateEditor = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, DauxHostCallbacks*, DauxEditor*, int>)&CreateEditor;
        vt->DestroyEditor = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, IntPtr, int>)&DestroyEditor;
        return (IntPtr)vt;
    }

    private static IntPtr BuildEditorVtable()
    {
        var vt = (DauxEditorVtable*)NativeMemory.AllocZeroed((nuint)sizeof(DauxEditorVtable));
        vt->StructSize = (uint)sizeof(DauxEditorVtable);
        vt->Attach = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, IntPtr, int>)&EditorAttach;
        vt->Detach = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&EditorDetach;
        vt->GetPreferredSize = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, DauxSize*, int>)&EditorGetPreferredSize;
        vt->SetBounds = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, DauxRect*, int>)&EditorSetBounds;
        vt->Show = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&EditorShow;
        vt->Hide = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&EditorHide;
        vt->Idle = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, void>)&EditorIdle;
        return (IntPtr)vt;
    }

    private static IntPtr BuildFactory()
    {
        var f = (DauxPluginFactory*)NativeMemory.AllocZeroed((nuint)sizeof(DauxPluginFactory));
        f->StructSize = (uint)sizeof(DauxPluginFactory);
        f->AbiVersion = Abi.AbiVersion;
        f->GetDescriptor = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr>)&GetDescriptor;
        f->Create = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, DauxPluginInstance*, int>)&Create;
        f->Destroy = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, int>)&Destroy;
        return (IntPtr)f;
    }

    // --- helpers ------------------------------------------------------------
    [MethodImpl(MethodImplOptions.AggressiveInlining)]
    private static IDauxPlugin? PluginFromHandle(IntPtr h)
    {
        if (h == IntPtr.Zero) return null;
        return GCHandle.FromIntPtr(h).Target as IDauxPlugin;
    }

    [MethodImpl(MethodImplOptions.AggressiveInlining)]
    private static IDauxEditor? EditorFromHandle(IntPtr h)
    {
        if (h == IntPtr.Zero) return null;
        return GCHandle.FromIntPtr(h).Target as IDauxEditor;
    }

    private static void WriteUtf8(byte* dst, int capacity, string s)
    {
        var bytes = System.Text.Encoding.UTF8.GetBytes(s ?? "");
        int n = Math.Min(bytes.Length, capacity - 1);
        for (int i = 0; i < n; i++) dst[i] = bytes[i];
        dst[n] = 0;
    }

    // --- factory thunks -----------------------------------------------------
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static IntPtr GetDescriptor() => _descriptorPtr;

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Create(IntPtr host, DauxPluginInstance* outInstance)
    {
        if (outInstance == null || _activator == null) return Abi.ErrInvalidArg;
        try
        {
            IDauxPlugin plugin = _activator();
            var gch = GCHandle.Alloc(plugin); // pin lifetime until Destroy
            outInstance->Handle = GCHandle.ToIntPtr(gch);
            outInstance->Vtable = _vtablePtr;
            return Abi.Ok;
        }
        catch
        {
            return Abi.ErrUnknown;
        }
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Destroy(IntPtr self)
    {
        if (self == IntPtr.Zero) return Abi.Ok;
        var gch = GCHandle.FromIntPtr(self);
        if (gch.Target is IDisposable d) d.Dispose();
        gch.Free();
        return Abi.Ok;
    }

    // --- vtable thunks ------------------------------------------------------
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Prepare(IntPtr self, double sampleRate, uint maxBlock)
    {
        var p = PluginFromHandle(self);
        if (p == null) return Abi.ErrInvalidState;
        p.Prepare(sampleRate, maxBlock);
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Activate(IntPtr self)
    {
        PluginFromHandle(self)?.Activate();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Deactivate(IntPtr self)
    {
        PluginFromHandle(self)?.Deactivate();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Reset(IntPtr self)
    {
        PluginFromHandle(self)?.Reset();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int Process(IntPtr self, DauxProcessData* data)
    {
        var p = PluginFromHandle(self);
        if (p == null || data == null) return Abi.ErrInvalidArg;

        ProcessContext ctx;
        if (data->Context != null)
        {
            DauxProcessContext* c = data->Context;
            ctx = new ProcessContext
            {
                SampleRate = c->SampleRate,
                BlockSize = c->BlockSize,
                TimelineSamples = c->TimelineSamples,
                TempoBpm = c->TempoBpm,
                TimeSignature = (c->TimeSigNumerator, c->TimeSigDenominator),
                IsPlaying = c->IsPlaying != 0,
                IsRecording = c->IsRecording != 0,
                IsLooping = c->IsLooping != 0,
            };
        }
        else
        {
            ctx = new ProcessContext { BlockSize = data->NumFrames, TempoBpm = 120.0, TimeSignature = (4, 4) };
        }

        p.Process(in ctx, new AudioBuffers(data));
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static uint GetParameterCount(IntPtr self) => (uint)_params.Length;

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int GetParameterInfo(IntPtr self, uint index, DauxParamInfo* outInfo)
    {
        if (outInfo == null) return Abi.ErrInvalidArg;
        if (index >= _params.Length) return Abi.ErrNotFound;
        ParamInfo pi = _params[index];

        NativeMemory.Clear(outInfo, (nuint)sizeof(DauxParamInfo));
        outInfo->Id = pi.Id;
        WriteUtf8(outInfo->Name, Abi.NameSize, pi.Name);
        WriteUtf8(outInfo->ShortName, Abi.ShortSize, string.IsNullOrEmpty(pi.ShortName) ? pi.Name : pi.ShortName);
        WriteUtf8(outInfo->Unit, Abi.ShortSize, pi.Unit);
        outInfo->DefaultValue = pi.Default;
        outInfo->MinValue = pi.Min;
        outInfo->MaxValue = pi.Max;
        outInfo->StepCount = pi.StepCount;
        outInfo->Flags = (int)pi.Flags;
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int GetParameterValue(IntPtr self, uint id, double* outValue)
    {
        var p = PluginFromHandle(self);
        if (p == null || outValue == null) return Abi.ErrInvalidArg;
        *outValue = p.GetParameter(id);
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int SetParameterValue(IntPtr self, uint id, double value)
    {
        var p = PluginFromHandle(self);
        if (p == null) return Abi.ErrInvalidState;
        p.SetParameter(id, value);
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int GetState(IntPtr self, void* outBuf, uint bufSize, uint* outSize)
    {
        var p = PluginFromHandle(self);
        if (p == null) return Abi.ErrInvalidState;
        byte[] blob = p.GetState() ?? Array.Empty<byte>();
        if (outSize != null) *outSize = (uint)blob.Length;
        if (outBuf == null || bufSize < blob.Length) return Abi.ErrBufferTooSmall;
        fixed (byte* src = blob)
            Buffer.MemoryCopy(src, outBuf, bufSize, blob.Length);
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int SetState(IntPtr self, void* data, uint size)
    {
        var p = PluginFromHandle(self);
        if (p == null || data == null) return Abi.ErrInvalidArg;
        var span = new ReadOnlySpan<byte>(data, (int)size);
        return p.SetState(span) ? Abi.Ok : Abi.ErrInvalidState;
    }

    // --- editor thunks ------------------------------------------------------
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int CreateEditor(IntPtr self, DauxHostCallbacks* host, DauxEditor* outEditor)
    {
        if (outEditor == null) return Abi.ErrInvalidArg;
        var p = PluginFromHandle(self);
        if (p == null) return Abi.ErrInvalidState;
        try
        {
            var bridge = new HostBridge(host);
            IDauxEditor? editor = p.CreateEditor(bridge);
            if (editor == null) return Abi.ErrNotSupported;
            var gch = GCHandle.Alloc(editor);
            outEditor->Handle = GCHandle.ToIntPtr(gch);
            outEditor->Vtable = _editorVtablePtr;
            return Abi.Ok;
        }
        catch
        {
            return Abi.ErrUnknown;
        }
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int DestroyEditor(IntPtr self, IntPtr editorHandle)
    {
        if (editorHandle == IntPtr.Zero) return Abi.Ok;
        var gch = GCHandle.FromIntPtr(editorHandle);
        (gch.Target as IDauxEditor)?.Dispose();
        gch.Free();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int EditorAttach(IntPtr ed, IntPtr parent)
    {
        var e = EditorFromHandle(ed);
        if (e == null) return Abi.ErrInvalidState;
        e.Attach(parent);
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int EditorDetach(IntPtr ed)
    {
        EditorFromHandle(ed)?.Detach();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int EditorGetPreferredSize(IntPtr ed, DauxSize* outSize)
    {
        var e = EditorFromHandle(ed);
        if (e == null || outSize == null) return Abi.ErrInvalidArg;
        EditorSize s = e.GetPreferredSize();
        outSize->Width = s.Width;
        outSize->Height = s.Height;
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int EditorSetBounds(IntPtr ed, DauxRect* bounds)
    {
        var e = EditorFromHandle(ed);
        if (e == null || bounds == null) return Abi.ErrInvalidArg;
        e.SetBounds(bounds->X, bounds->Y, bounds->Width, bounds->Height);
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int EditorShow(IntPtr ed)
    {
        EditorFromHandle(ed)?.Show();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static int EditorHide(IntPtr ed)
    {
        EditorFromHandle(ed)?.Hide();
        return Abi.Ok;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static void EditorIdle(IntPtr ed)
    {
        EditorFromHandle(ed)?.Idle();
    }
}
