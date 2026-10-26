param([int]$ProcessId)

$source = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class WindowProbe
{
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumProc callback, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int count);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern int GetWindowLongW(IntPtr hWnd, int index);

    public static List<string> List(uint wanted)
    {
        var found = new List<string>();
        EnumWindows((handle, unused) =>
        {
            uint owner;
            GetWindowThreadProcessId(handle, out owner);
            if (owner == wanted)
            {
                var text = new StringBuilder(512);
                GetWindowTextW(handle, text, text.Capacity);
                var style = GetWindowLongW(handle, -16);
                found.Add("hwnd=" + handle.ToInt64()
                    + " visible=" + IsWindowVisible(handle)
                    + " style=0x" + ((uint)style).ToString("X8")
                    + " title=\"" + text + "\"");
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@

Add-Type -TypeDefinition $source -Language CSharp
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
[WindowProbe]::List([uint32]$ProcessId)
