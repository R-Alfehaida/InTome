# Brings a top-level window to the foreground and presses real keys through
# SendInput (WScript SendKeys drops arrow keys in some sessions).
# Keys: comma-separated chords such as "down,down,tab,shift+tab,alt+up,space,enter,esc,a".
param([Parameter(Mandatory)][string]$WindowTitle, [Parameter(Mandatory)][string]$Keys, [int]$DelayMs = 120)
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class Press {
  [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
  [StructLayout(LayoutKind.Explicit, Size = 40)] struct INPUT { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KEYBDINPUT ki; }
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string t);
  public static IntPtr Find(string t) { return FindWindowW(null, t); }
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  static INPUT Key(ushort vk, bool up) {
    var i = new INPUT(); i.type = 1; i.ki.vk = vk;
    bool ext = vk == 0x25 || vk == 0x26 || vk == 0x27 || vk == 0x28 || vk == 0x24 || vk == 0x23 || vk == 0x2E || vk == 0x2D;
    i.ki.flags = (up ? 2u : 0u) | (ext ? 1u : 0u); return i;
  }
  public static void Chord(ushort[] mods, ushort vk) {
    var list = new System.Collections.Generic.List<INPUT>();
    foreach (var m in mods) list.Add(Key(m, false));
    list.Add(Key(vk, false)); list.Add(Key(vk, true));
    for (int k = mods.Length - 1; k >= 0; k--) list.Add(Key(mods[k], true));
    SendInput((uint)list.Count, list.ToArray(), Marshal.SizeOf(typeof(INPUT)));
  }
}
'@
$vk = @{ tab=0x09; enter=0x0D; esc=0x1B; space=0x20; end=0x23; home=0x24; left=0x25; up=0x26; right=0x27; down=0x28; delete=0x2E; f4=0x73 }
$mod = @{ shift=0x10; ctrl=0x11; alt=0x12 }
$h = [Press]::Find($WindowTitle)
if ($h -eq [IntPtr]::Zero) { Write-Output "FAIL: window '$WindowTitle' not found"; exit 2 }
if ([Press]::GetForegroundWindow() -ne $h) {
    # Alt tap lets this process take the foreground (SetForegroundWindow lock rule).
    [Press]::Chord(@(), 0x12); [void][Press]::SetForegroundWindow($h); Start-Sleep -Milliseconds 200
}
foreach ($chord in $Keys.Split(',')) {
    $parts = $chord.Trim().ToLower().Split('+')
    $mods = [uint16[]]@(if ($parts.Length -gt 1) { $parts[0..($parts.Length - 2)] | ForEach-Object { $mod[$_] } })
    $last = $parts[-1]
    $code = if ($vk.ContainsKey($last)) { $vk[$last] } else { [int][char]$last.ToUpper() }
    [Press]::Chord($mods, [uint16]$code)
    Start-Sleep -Milliseconds $DelayMs
}
