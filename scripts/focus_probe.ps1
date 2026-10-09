# Reports which control has keyboard focus in a window's GUI thread
# (GetGUIThreadInfo), with its class, text and MSAA name/role, for keyboard checks.
param([Parameter(Mandatory)][string]$WindowTitle)
Add-Type -ReferencedAssemblies Accessibility -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text; using Accessibility;
public static class FocusProbe {
  [StructLayout(LayoutKind.Sequential)] struct RECT { public int l, t, r, b; }
  [StructLayout(LayoutKind.Sequential)] struct GTI { public int cb; public int flags; public IntPtr act, foc, cap, menu, move, caret; public RECT rc; }
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint tid, ref GTI g);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("oleacc.dll")] static extern int AccessibleObjectFromWindow(IntPtr h, uint id, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out object o);
  [DllImport("oleacc.dll", CharSet = CharSet.Unicode)] static extern uint GetRoleTextW(uint role, StringBuilder s, uint n);
  public static string Probe(string title) {
    var h = FindWindowW(null, title); if (h == IntPtr.Zero) return "FAIL: no window '" + title + "'";
    var g = new GTI(); g.cb = Marshal.SizeOf(g); GetGUIThreadInfo(GetWindowThreadProcessId(h, IntPtr.Zero), ref g);
    if (g.foc == IntPtr.Zero) return "focus=<none>";
    var c = new StringBuilder(128); GetClassNameW(g.foc, c, 128);
    var t = new StringBuilder(256); GetWindowTextW(g.foc, t, 256);
    string name = "", role = "?";
    Guid iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71"); object o;
    if (AccessibleObjectFromWindow(g.foc, 0xFFFFFFFC, ref iid, out o) == 0) {
      var acc = (IAccessible)o;
      try { name = acc.get_accName(0) ?? ""; } catch {}
      try { var rs = new StringBuilder(64); GetRoleTextW(Convert.ToUInt32(acc.get_accRole(0)), rs, 64); role = rs.ToString(); } catch {}
    }
    return "focus=" + c + " role=" + role + " name='" + name + "' text='" + t + "'";
  }
}
'@
[FocusProbe]::Probe($WindowTitle)
