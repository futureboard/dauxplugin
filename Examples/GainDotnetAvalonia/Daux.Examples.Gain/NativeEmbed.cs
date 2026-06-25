// NativeEmbed.cs - Win32 helpers for embedding the editor window into the
// host's native parent window. LibraryImport (source-generated) is used so the
// calls remain NativeAOT-friendly.
using System.Runtime.InteropServices;

namespace Daux.Examples.Gain;

internal static partial class NativeEmbed
{
    [LibraryImport("user32.dll", SetLastError = true)]
    public static partial IntPtr SetParent(IntPtr hWndChild, IntPtr hWndNewParent);

    // GWL_STYLE / WS_CHILD bits, for turning the top-level window into a child.
    // TODO: full embedding also needs to clear WS_POPUP/caption and resize to
    // the parent client rect; left as a follow-up (see dauxhost-protocol.md).
}
