# Runs the page-navigation scenarios under NVDA in Prism and InTome, with
# continuous reading off and on, writing what NVDA spoke to <Out>\<scenario>-<mode>-<app>.txt.
# Needs a release build of InTome. Keep hands off the keyboard while it runs.
#
#   nvda_matrix.ps1 -Out C:\temp\nvda -Apps intome -Only boundary,sections
param(
    [string]$Out = (Join-Path $env:TEMP 'nvda-matrix'),
    [string[]]$Apps = @('prism', 'intome'),
    [string[]]$Modes = @('off', 'on'),
    [string[]]$Only
)
$fixtures = Join-Path $PSScriptRoot '..\tests\fixtures' | Resolve-Path
$toc = Join-Path $fixtures 'text_toc.pdf'
$epub = Join-Path $fixtures 'book.epub'
# 12 chapters x 3 sections x 5 paragraphs: about 68k chars, 25 pages at the default page size.
$long = Join-Path $env:TEMP 'nvda-matrix-long.md'
$words = 'the reader moves through each chapter slowly while the river keeps its quiet course past old stone bridges and narrow lanes'.Split(' ')
$rng = New-Object System.Random 7
$md = New-Object System.Text.StringBuilder
[void]$md.Append("# The Long Book`n`n")
foreach ($ch in 1..12) {
    [void]$md.Append("## Chapter $ch`n`n")
    foreach ($sec in 1..3) {
        [void]$md.Append("### Section $ch.$sec`n`n")
        foreach ($p in 1..5) { [void]$md.Append("Paragraph $ch.$sec.$p. " + ((1..60 | ForEach-Object { $words[$rng.Next($words.Count)] }) -join ' ') + ".`n`n") }
    }
}
[IO.File]::WriteAllText($long, $md.ToString())

$scenarios = [ordered]@{
    single   = @($toc, 'pagedown;pagedown;pageup;pageup')
    rapid    = @($long, 'pagedown,pagedown,pagedown,pagedown,pagedown@3000;pageup@2000')
    ends     = @($long, 'ctrl+end@2500;ctrl+home@2500')
    sections = @($toc, 'alt+pagedown;alt+pagedown;alt+pageup')
    epubsec  = @($epub, 'alt+pagedown;alt+pagedown;alt+pageup')
    toc      = @($toc, 'shift+f6;down;down;enter@2500')
    arrows   = @($toc, 'down@700;down@700;down@700;down@700;down@700;down@700;down@700;down@700;up@700;up@700')
    boundary = @($toc, 'pagedown;up;down@1200;down@1500;down@1500;down@1500')
    headings = @($long, 'h;h;h;h;h;h')
    sayall   = @($toc, 'insert+down@25000;insert+up@1500')
}
New-Item -ItemType Directory -Force $Out | Out-Null
foreach ($name in $scenarios.Keys) {
    if ($Only -and $name -notin $Only) { continue }
    foreach ($mode in $Modes) {
        foreach ($app in $Apps) {
            $doc, $steps = $scenarios[$name]
            & (Join-Path $PSScriptRoot 'nvda_scenario.ps1') -App $app -Doc $doc -Steps $steps -Continuous $mode *>&1 |
                Out-File -Encoding utf8 (Join-Path $Out "$name-$mode-$app.txt")
        }
    }
}
