// Entry.cs - the single native export for this plugin.
//
// NativeAOT turns this [UnmanagedCallersOnly] static method into the exported
// C symbol `daux_plugin_entry`, which the DAUx host loader resolves. It just
// forwards to DauxRuntime with this plugin's descriptor, parameter table and
// activator (defined in Daux.Examples.Gain).
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using Daux.Examples.Gain;
using Daux.Plugin;

namespace Daux.Examples.Gain.NativeShell;

public static class Entry
{
    [UnmanagedCallersOnly(EntryPoint = "daux_plugin_entry", CallConvs = new[] { typeof(CallConvCdecl) })]
    public static IntPtr DauxPluginEntry(uint hostAbiVersion)
    {
        return DauxRuntime.GetEntry(
            hostAbiVersion,
            GainPlugin.Descriptor,
            GainPlugin.Parameters,
            static () => new GainPlugin());
    }
}
