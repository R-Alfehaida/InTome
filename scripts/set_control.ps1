# Data entry for UI checks: finds a control in a top-level window by its MSAA
# name and sets it the way a user's input would leave it.
#   -Text sets an edit's text (WM_SETTEXT); -Check 1/0 sets a check box (BM_SETCHECK);
#   -Select picks a combo box or list box entry by its text (CB_/LB_SELECTSTRING).
# Prints the control's class and name, or FAIL.
param(
    [Parameter(Mandatory)][string]$WindowTitle,
    [Parameter(Mandatory)][string]$Name,
    [string]$Text,
    [int]$Check = -1,
    [string]$Select
)
Add-Type -ReferencedAssemblies Accessibility -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text; using Accessibility;
public static class SetCtl {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr h, EnumProc f, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, string l);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("oleacc.dll")] static extern int AccessibleObjectFromWindow(IntPtr h, uint id, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out object o);
  public static IntPtr Top(string t) { return FindWindowW(null, t); }
  public static string Cls(IntPtr h) { var c = new StringBuilder(128); GetClassNameW(h, c, 128); return c.ToString(); }
  public static IntPtr Find(IntPtr top, string name) {
    IntPtr found = IntPtr.Zero;
    EnumChildWindows(top, (h, l) => {
      Guid iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71"); object o;
      if (AccessibleObjectFromWindow(h, 0xFFFFFFFC, ref iid, out o) == 0) {
        string n = ""; try { n = ((IAccessible)o).get_accName(0) ?? ""; } catch {}
        var c = Cls(h);
        if (n == name && c != "Static" && !(c == "Button" && name.EndsWith(":"))) { found = h; return false; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@
$top = [SetCtl]::Top($WindowTitle)
if ($top -eq [IntPtr]::Zero) { Write-Output "FAIL: window '$WindowTitle' not found"; exit 2 }
$h = [SetCtl]::Find($top, $Name)
if ($h -eq [IntPtr]::Zero) { Write-Output "FAIL: no control named '$Name'"; exit 1 }
$cls = [SetCtl]::Cls($h)
if ($PSBoundParameters.ContainsKey('Text')) { [void][SetCtl]::SendMessageW($h, 0x000C, [IntPtr]::Zero, $Text) }
if ($Check -ge 0) { [void][SetCtl]::SendMessageW($h, 0x00F1, [IntPtr]$Check, [IntPtr]::Zero) }
if ($Select) {
    $msg = if ($cls -eq 'ComboBox') { 0x014D } else { 0x018C }
    $r = [SetCtl]::SendMessageW($h, $msg, [IntPtr](-1), $Select)
    if ([int64]$r -lt 0) { Write-Output "FAIL: '$Select' not in '$Name'"; exit 1 }
}
Write-Output "ok  $cls '$Name'"
