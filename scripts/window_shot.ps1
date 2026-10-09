# Saves a PNG of a top-level window (by title) for layout checks. Uses
# PrintWindow, so the image is right even when another window covers it.
param([Parameter(Mandatory)][string]$WindowTitle, [Parameter(Mandatory)][string]$Out)
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class Shot {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int l, t, r, b; }
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string t);
  public static IntPtr Find(string t) { return FindWindowW(null, t); }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[void][Shot]::SetProcessDPIAware()
$h = [Shot]::Find($WindowTitle)
if ($h -eq [IntPtr]::Zero) { Write-Output "FAIL: window '$WindowTitle' not found"; exit 2 }
$r = New-Object Shot+RECT; [void][Shot]::GetWindowRect($h, [ref]$r)
$bmp = New-Object System.Drawing.Bitmap ($r.r - $r.l), ($r.b - $r.t)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc(); [void][Shot]::PrintWindow($h, $hdc, 2); $g.ReleaseHdc($hdc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "saved $Out"
