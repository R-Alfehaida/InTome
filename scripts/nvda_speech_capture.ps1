# Reads back exactly what NVDA spoke, in order, from the Speech History add-on
# (jscholes/nvda-speech-history, installed in this NVDA). One line per
# speech.speak call; parts of one call are joined by two spaces, as in NVDA's
# Speech Viewer. NVDA's desktop keyboard layout is assumed.
#
#   nvda_speech_capture.ps1 -Mark [-Text t]   speak a marker line through nvdaControllerClient
#   nvda_speech_capture.ps1 -Dump             print NVDA's history after the last marker
#   nvda_speech_capture.ps1 -Close            close a history window a failed -Dump left open
#
# Why this route: NVDA runs with uiAccess, so UIPI drops this process's input
# and window messages to every NVDA window, and while an NVDA window is in the
# foreground injected keys do not reach NVDA at all. The Speech Viewer cannot
# be opened from a script. -Dump instead:
#   1. NVDA+H opens the add-on's history window (it takes the foreground).
#   2. A child exe run under this process as debugger takes the foreground
#      back (SetForegroundWindow is allowed for a debugged process).
#   3. With an ordinary window in front, NVDA gestures work again: object
#      navigation finds the history document, NVDA's review cursor copies its
#      text (NVDA+F9, NVDA+F10 twice), and NVDA itself presses the window's
#      Close button. F12 (the add-on's copy-last-utterance) reports where the
#      navigator is after each move.
# The clipboard is overwritten.
param([switch]$Mark, [string]$Text = 'capture mark', [switch]$Dump, [switch]$Close)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
# InTome speaks through prism and no longer ships this DLL; the markers borrow
# accessible_output2's copy (see nvda_locate.ps1 for where it looks).
. (Join-Path $PSScriptRoot 'nvda_locate.ps1')
$dll = Find-NvdaClientDll
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class NvdaCap {
  [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
  [StructLayout(LayoutKind.Explicit, Size = 40)] struct INPUT { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KEYBDINPUT ki; }
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll")] static extern uint MapVirtualKeyW(uint c, uint t);
  [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport(@"$dll", CharSet = CharSet.Unicode)] public static extern int nvdaController_speakText(string t);
  static void K(ushort vk, bool up, bool ext) {
    var i = new INPUT(); i.type = 1; i.ki.vk = vk; i.ki.scan = (ushort)MapVirtualKeyW(vk, 0); i.ki.flags = (up ? 2u : 0u) | (ext ? 1u : 0u);
    SendInput(1, new[] { i }, Marshal.SizeOf(typeof(INPUT))); System.Threading.Thread.Sleep(25);
  }
  // mods: 1 = NVDA (Insert), 2 = Shift.
  public static void Press(int mods, ushort vk, bool ext) {
    if ((mods & 1) != 0) K(0x2D, false, true);
    if ((mods & 2) != 0) K(0x10, false, false);
    K(vk, false, ext); K(vk, true, ext);
    if ((mods & 2) != 0) K(0x10, true, false);
    if ((mods & 1) != 0) K(0x2D, true, true);
  }
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct STARTUPINFO { public int cb; public string r, d, t; public int x, y, w, h, cx, cy, fill, flags; public short show, r2; public IntPtr r3, i, o, e; }
  [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int pid, tid; }
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool CreateProcessW(string app, string cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
  [DllImport("kernel32.dll")] static extern bool WaitForDebugEvent(byte[] ev, uint ms);
  [DllImport("kernel32.dll")] static extern bool ContinueDebugEvent(int pid, int tid, uint status);
  // Runs exe under this process as its debugger until it exits.
  public static bool RunDebugged(string exe, string arg) {
    var si = new STARTUPINFO(); si.cb = Marshal.SizeOf(si); PROCESS_INFORMATION pi;
    if (!CreateProcessW(exe, "\"" + exe + "\" " + arg, IntPtr.Zero, IntPtr.Zero, false, 0x2, IntPtr.Zero, null, ref si, out pi)) return false;
    var ev = new byte[256];
    while (WaitForDebugEvent(ev, 15000)) {
      int code = BitConverter.ToInt32(ev, 0);
      uint status = 0x00010002;
      if (code == 1) { uint exc = BitConverter.ToUInt32(ev, 16); if (exc != 0x80000003 && exc != 0x4000001F) status = 0x80010001; }
      ContinueDebugEvent(BitConverter.ToInt32(ev, 4), BitConverter.ToInt32(ev, 8), status);
      if (code == 5) return true;
    }
    return false;
  }
}
"@

function Say-Last {
    $seq = [NvdaCap]::GetClipboardSequenceNumber()
    for ($try = 0; $try -lt 4; $try++) {
        [NvdaCap]::Press(0, 0x7B, $false)
        $deadline = (Get-Date).AddMilliseconds(700)
        while ([NvdaCap]::GetClipboardSequenceNumber() -eq $seq -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 30 }
        if ([NvdaCap]::GetClipboardSequenceNumber() -ne $seq) { Start-Sleep -Milliseconds 40; return [System.Windows.Forms.Clipboard]::GetText() }
    }
    throw 'F12 (copy last utterance) did not reach the clipboard'
}
# Desktop-layout object navigation: NVDA+numpad8/2/4/6 with NumLock off.
$nav = @{ parent = 0x26; child = 0x28; prev = 0x25; next = 0x27 }
function Move-Nav([string]$Dir) { [NvdaCap]::Press(1, [uint16]$nav[$Dir], $false); Start-Sleep -Milliseconds 300; Say-Last }
# Unrelated speech (a notification) can land between a move and F12; NVDA+numpad5 re-reports the navigator.
function Get-Here { [NvdaCap]::Press(1, 0x0C, $false); Start-Sleep -Milliseconds 300; Say-Last }
function Move-Until([string]$Dir, [string]$Pattern, [int]$Max = 15) {
    for ($i = 0; $i -lt $Max; $i++) { $t = Move-Nav $Dir; if ($t -match $Pattern -or ($t = Get-Here) -match $Pattern) { return } }
    throw "navigator never reached /$Pattern/ (last: $t)"
}

# Moves the navigator onto the add-on's history window, searching the top-level windows from the Desktop.
function Enter-HistoryWindow {
    Move-Until parent '^Desktop'
    $t = Move-Nav child
    for ($i = 0; $t -notmatch '^Speech History  window' -and ($t = Get-Here) -notmatch '^Speech History  window'; $i++) {
        if ($i -ge 60) { throw 'the Speech History window is not among the top-level windows' }
        $t = Move-Nav next
    }
}
# NVDA itself presses the window's Close button.
function Close-HistoryWindow {
    Enter-HistoryWindow
    Move-Until child 'menu bar' 1
    Move-Until next '^Close  button' 1
    [NvdaCap]::Press(1, 0x0D, $true)
    Start-Sleep -Milliseconds 500
}

if ($Mark) {
    [void][NvdaCap]::nvdaController_speakText($Text)
    Start-Sleep -Milliseconds 400
    exit 0
}
if ($Close) {
    Close-HistoryWindow
    exit 0
}
if ($Dump) {
    $front = [NvdaCap]::GetForegroundWindow()
    $fgset = Join-Path $env:TEMP 'nvda_capture_fgset.exe'
    if (-not (Test-Path $fgset)) {
        Add-Type -OutputAssembly $fgset -OutputType ConsoleApplication -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class P {
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
  public static int Main(string[] a) { return SetForegroundWindow(new IntPtr(Convert.ToInt64(a[0]))) ? 0 : 1; }
}
"@
    }
    [NvdaCap]::Press(1, 0x48, $false)
    Start-Sleep -Milliseconds 1500
    if (-not [NvdaCap]::RunDebugged($fgset, [string]$front.ToInt64())) { throw 'could not take the foreground back from NVDA' }
    Start-Sleep -Milliseconds 500
    try {
        Enter-HistoryWindow
        Move-Until child 'menu bar' 1
        Move-Until next '^Close  button' 1
        Move-Until next '^Speech History  document' 1
        # Review cursor: top line, mark, bottom line, end of line, copy (NVDA+F10 twice).
        [NvdaCap]::Press(2, 0x24, $false); Start-Sleep -Milliseconds 200
        [NvdaCap]::Press(1, 0x78, $false); Start-Sleep -Milliseconds 200
        [NvdaCap]::Press(2, 0x21, $false); Start-Sleep -Milliseconds 200
        [NvdaCap]::Press(2, 0x22, $false); Start-Sleep -Milliseconds 200
        $seq = [NvdaCap]::GetClipboardSequenceNumber()
        [NvdaCap]::Press(1, 0x79, $false); Start-Sleep -Milliseconds 100; [NvdaCap]::Press(1, 0x79, $false)
        $deadline = (Get-Date).AddMilliseconds(3000)
        while ([NvdaCap]::GetClipboardSequenceNumber() -eq $seq -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 50 }
        Start-Sleep -Milliseconds 100
        $history = if ([NvdaCap]::GetClipboardSequenceNumber() -ne $seq) { [System.Windows.Forms.Clipboard]::GetText() } else { '' }
    } finally {
        Close-HistoryWindow
    }
    # The window lists the newest utterance first.
    $lines = @($history -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    [array]::Reverse($lines)
    $start = [array]::LastIndexOf($lines, $Text)
    if ($start -lt 0) { Write-Output '<capture: marker not found; full history follows>'; $lines; exit 1 }
    if ($start + 1 -lt $lines.Count) { $lines[($start + 1)..($lines.Count - 1)] }
    exit 0
}
Write-Output 'usage: nvda_speech_capture.ps1 -Mark [-Text t] | -Dump | -Close'
exit 2
