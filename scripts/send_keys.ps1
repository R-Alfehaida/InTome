# Focuses a top-level window by title and sends keys (SendKeys syntax), for keyboard checks.
param([Parameter(Mandatory)][string]$WindowTitle, [Parameter(Mandatory)][string]$Keys, [int]$DelayMs = 300)
$sh = New-Object -ComObject WScript.Shell
if (-not $sh.AppActivate($WindowTitle)) { Write-Output "FAIL: cannot activate '$WindowTitle'"; exit 2 }
Start-Sleep -Milliseconds $DelayMs
$sh.SendKeys($Keys)
Start-Sleep -Milliseconds $DelayMs
