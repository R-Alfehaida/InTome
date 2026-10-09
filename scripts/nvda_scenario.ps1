# Opens a document in Prism or InTome under NVDA, runs a key sequence, and
# prints what NVDA spoke for each step, in order (see nvda_speech_capture.ps1).
#
#   nvda_scenario.ps1 -App intome -Doc tests\fixtures\text_toc.pdf -Steps "pagedown;pagedown,pagedown@2500"
#
# Steps are separated by ';'. A step is comma-separated chords (press_keys.ps1
# syntax) pressed -KeyDelayMs apart, then a wait of -StepMs, or of the
# step's own "@ms" suffix. Each app runs with APPDATA pointed at a fresh
# scratch folder holding only `continuous_reading`, so the user's settings,
# recent files and saved reading positions are never read or written.
param(
    [Parameter(Mandatory)][ValidateSet('prism', 'intome')][string]$App,
    [Parameter(Mandatory)][string]$Doc,
    [Parameter(Mandatory)][string]$Steps,
    [ValidateSet('on', 'off')][string]$Continuous = 'off',
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\intome.exe'),
    [string]$PrismRoot = 'C:\Users\khais\Documents\GitHub\Prism-Document-Reader',
    [int]$KeyDelayMs = 120,
    [int]$StepMs = 1500,
    [int]$LoadMs = 5000
)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Text;
public static class ScenarioWin {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  public static string FindTitle(string part) {
    string found = null;
    EnumWindows((h, l) => {
      var t = new StringBuilder(512); GetWindowTextW(h, t, 512);
      if (IsWindowVisible(h) && t.ToString().EndsWith(part)) { found = t.ToString(); return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  // A key tap makes this process the last to receive input, which lets it take
  // the foreground. F24, because an Alt tap would open the app's menu bar.
  public static void Focus(IntPtr h) { if (GetForegroundWindow() == h) return; keybd_event(0x87, 0, 0, IntPtr.Zero); keybd_event(0x87, 0, 2, IntPtr.Zero); SetForegroundWindow(h); }
}
'@
$capture = Join-Path $PSScriptRoot 'nvda_speech_capture.ps1'
$press = Join-Path $PSScriptRoot 'press_keys.ps1'
$doc = (Resolve-Path $Doc).Path
$leaf = Split-Path $doc -Leaf
# A loaded document titles the window "<doc title> - <app>".
$suffix = if ($App -eq 'intome') { ' - InTome' } else { ' - Prism' }
$on = if ($Continuous -eq 'on') { 'true' } else { 'false' }

$appdata = Join-Path $env:TEMP "nvda-scenario\$App"
Remove-Item -Recurse -Force $appdata -ErrorAction SilentlyContinue
if ($App -eq 'intome') {
    New-Item -ItemType Directory -Force (Join-Path $appdata 'InTome') | Out-Null
    Set-Content -Encoding ascii (Join-Path $appdata 'InTome\intome.toml') "[general]`ncontinuous_reading = $on`n"
} else {
    New-Item -ItemType Directory -Force (Join-Path $appdata 'Prism') | Out-Null
    Set-Content -Encoding ascii (Join-Path $appdata 'Prism\prism.ini') "[general]`ncontinuous_reading = $on`n"
}

$savedAppdata = $env:APPDATA
$env:APPDATA = $appdata
& $capture -Mark
try {
    $proc = if ($App -eq 'intome') {
        Start-Process -FilePath (Resolve-Path $Exe).Path -ArgumentList "`"$doc`"" -PassThru
    } else {
        Start-Process -FilePath (Join-Path $PrismRoot '.venv\Scripts\python.exe') -ArgumentList '-m', 'prism', "`"$doc`"" -WorkingDirectory $PrismRoot -PassThru
    }
} finally {
    $env:APPDATA = $savedAppdata
}

$title = $null
$deadline = (Get-Date).AddSeconds(30)
while (-not $title -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250; $title = [ScenarioWin]::FindTitle($suffix) }
if (-not $title) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue; Write-Output "FAIL: no window titled *$suffix*"; exit 2 }
# NVDA mouse tracking speaks whatever changes under the cursor; park it in the screen corner.
[void][ScenarioWin]::SetProcessDPIAware()
$hwnd = [ScenarioWin]::FindWindowW($null, $title)
[ScenarioWin]::Focus($hwnd)
[void][ScenarioWin]::SetCursorPos(10000, 10000)
Start-Sleep -Milliseconds $LoadMs

# Each step is labelled in NVDA's own history by a spoken "step: <keys>" line.
foreach ($step in $Steps.Split(';')) {
    $keys, $wait = $step.Trim().Split('@')
    $wait = if ($wait) { [int]$wait } else { $StepMs }
    & $capture -Mark -Text "step: $keys"
    & $press -WindowTitle $title -Keys $keys -DelayMs $KeyDelayMs | Out-Null
    Start-Sleep -Milliseconds $wait
}

Write-Output "== $App, continuous $Continuous, $leaf"
Write-Output "-- open"
& $capture -Dump | ForEach-Object { if ($_ -like 'step: *') { "-- " + $_.Substring(6) } else { "   $_" } }

Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
if ($App -eq 'prism') { Get-CimInstance Win32_Process -Filter "ParentProcessId=$($proc.Id)" | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue } }
