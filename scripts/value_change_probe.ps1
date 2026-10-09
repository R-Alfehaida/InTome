# Checks that a window's text controls tell screen readers when their text is
# replaced: hooks EVENT_OBJECT_VALUECHANGE for the window's process, presses
# one key (SendInput), pumps the hook's events, then prints every
# value-change event seen and the focused control's MSAA value (accValue),
# which is what NVDA and JAWS read back.
param(
    [Parameter(Mandatory)][string]$WindowTitle,
    [ValidateSet('pagedown', 'pageup', 'home', 'end')][string]$Key = 'pagedown',
    [switch]$Ctrl,
    [int]$Chars = 160
)
Add-Type -ReferencedAssemblies Accessibility -TypeDefinition @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Text; using Accessibility;
public static class ValueProbe {
  delegate void WinEventProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
  [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc proc, uint pid, uint tid, uint flags);
  [DllImport("user32.dll")] static extern bool UnhookWinEvent(IntPtr hook);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr h; public uint m; public IntPtr w, l; public uint t; public int x, y; }
  [DllImport("user32.dll")] static extern bool PeekMessageW(out MSG m, IntPtr h, uint a, uint b, uint remove);
  [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG m);
  [DllImport("user32.dll")] static extern IntPtr DispatchMessageW(ref MSG m);
  [StructLayout(LayoutKind.Sequential)] struct RECT { public int l, t, r, b; }
  [StructLayout(LayoutKind.Sequential)] struct GTI { public int cb; public int flags; public IntPtr act, foc, cap, menu, move, caret; public RECT rc; }
  [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint tid, ref GTI g);
  [DllImport("oleacc.dll")] static extern int AccessibleObjectFromWindow(IntPtr h, uint id, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out object o);
  [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
  [StructLayout(LayoutKind.Explicit, Size = 40)] struct INPUT { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KEYBDINPUT ki; }
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);

  static INPUT K(ushort vk, bool up, bool ext) { var i = new INPUT(); i.type = 1; i.ki.vk = vk; i.ki.flags = (up ? 2u : 0u) | (ext ? 1u : 0u); return i; }
  static string Cls(IntPtr h) { var s = new StringBuilder(128); GetClassNameW(h, s, 128); return s.ToString(); }

  public static string Value(IntPtr h, int chars) {
    Guid iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71"); object o;
    if (AccessibleObjectFromWindow(h, 0xFFFFFFFC, ref iid, out o) != 0) return "<no IAccessible>";
    string v = ""; try { v = ((IAccessible)o).get_accValue(0) ?? ""; } catch {}
    v = v.Replace("\r", "\\r").Replace("\n", "\\n");
    return v.Length > chars ? v.Substring(0, chars) + "..." : v;
  }

  public static string Run(string title, ushort vk, bool ctrl, int chars) {
    var top = FindWindowW(null, title); if (top == IntPtr.Zero) return "FAIL: no window '" + title + "'";
    uint pid; uint tid = GetWindowThreadProcessId(top, out pid);
    var seen = new List<string>();
    WinEventProc proc = (hook, ev, hwnd, obj, child, th, time) => { if (obj == -4) seen.Add(Cls(hwnd) + " 0x" + hwnd.ToString("X")); };
    var hk = SetWinEventHook(0x800E, 0x800E, IntPtr.Zero, proc, pid, 0, 0);
    SetForegroundWindow(top);
    var g = new GTI(); g.cb = Marshal.SizeOf(g); GetGUIThreadInfo(tid, ref g);
    string before = Value(g.foc, chars);
    var keys = new List<INPUT>();
    if (ctrl) keys.Add(K(0x11, false, false));
    keys.Add(K(vk, false, true)); keys.Add(K(vk, true, true));
    if (ctrl) keys.Add(K(0x11, true, false));
    SendInput((uint)keys.Count, keys.ToArray(), Marshal.SizeOf(typeof(INPUT)));
    var end = DateTime.Now.AddMilliseconds(1500); MSG m;
    while (DateTime.Now < end) { while (PeekMessageW(out m, IntPtr.Zero, 0, 0, 1)) { TranslateMessage(ref m); DispatchMessageW(ref m); } System.Threading.Thread.Sleep(20); }
    UnhookWinEvent(hk); GC.KeepAlive(proc);
    GetGUIThreadInfo(tid, ref g);
    return "focus=" + Cls(g.foc) + " 0x" + g.foc.ToString("X") + "\nvalue before: " + before + "\nvalue-change events (OBJID_CLIENT): " + (seen.Count == 0 ? "none" : string.Join(", ", seen)) + "\nvalue after:  " + Value(g.foc, chars);
  }
}
'@
$codes = @{ pagedown = 0x22; pageup = 0x21; home = 0x24; end = 0x23 }
[ValueProbe]::Run($WindowTitle, [uint16]$codes[$Key], $Ctrl.IsPresent, $Chars)
