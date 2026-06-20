using System.Runtime.InteropServices;
using System.Text;

namespace DocxyCompare;

/// <summary>Thin Win32 wrappers for positioning the launcher, the docxy console,
/// and the Word window (all in physical pixels, so no DPI conversion is needed).</summary>
internal static partial class Win32
{
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    public readonly record struct Area(int Left, int Top, int Width, int Height);

    public const uint SWP_NOZORDER = 0x0004;
    public const uint SWP_NOACTIVATE = 0x0010;
    public const uint SWP_SHOWWINDOW = 0x0040;
    public const int SW_RESTORE = 9;
    private const uint SPI_GETWORKAREA = 0x0030;

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SystemParametersInfoW(uint uiAction, uint uiParam, ref RECT pvParam, uint fWinIni);

    [LibraryImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool SetForegroundWindow(IntPtr hWnd);

    private delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool EnumWindows(EnumProc cb, IntPtr lParam);

    [LibraryImport("user32.dll", EntryPoint = "GetClassNameW", StringMarshalling = StringMarshalling.Utf16)]
    private static partial int GetClassName(IntPtr hWnd, [Out] char[] buf, int max);

    [LibraryImport("user32.dll", EntryPoint = "GetWindowTextW", StringMarshalling = StringMarshalling.Utf16)]
    private static partial int GetWindowText(IntPtr hWnd, [Out] char[] buf, int max);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool IsWindowVisible(IntPtr hWnd);

    /// <summary>Work area of the primary monitor (minus the taskbar), in pixels.</summary>
    public static Area WorkArea()
    {
        RECT r = default;
        if (SystemParametersInfoW(SPI_GETWORKAREA, 0, ref r, 0))
            return new Area(r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top);
        return new Area(0, 0, 1920, 1040);
    }

    public static void Place(IntPtr hWnd, int x, int y, int w, int h)
    {
        if (hWnd == IntPtr.Zero) return;
        ShowWindow(hWnd, SW_RESTORE);
        SetWindowPos(hWnd, IntPtr.Zero, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }

    /// <summary>Find a top-level window of <paramref name="className"/> whose title
    /// contains <paramref name="titlePart"/> (case-insensitive).</summary>
    public static IntPtr FindWindowByClassAndTitle(string className, string titlePart)
    {
        IntPtr found = IntPtr.Zero;
        var cbuf = new char[256];
        var tbuf = new char[512];
        EnumWindows((h, _) =>
        {
            if (!IsWindowVisible(h)) return true;
            int cn = GetClassName(h, cbuf, cbuf.Length);
            if (cn <= 0) return true;
            var cls = new string(cbuf, 0, cn);
            if (!string.Equals(cls, className, StringComparison.OrdinalIgnoreCase)) return true;
            int tn = GetWindowText(h, tbuf, tbuf.Length);
            var title = tn > 0 ? new string(tbuf, 0, tn) : "";
            if (title.Contains(titlePart, StringComparison.OrdinalIgnoreCase))
            {
                found = h;
                return false; // stop
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
