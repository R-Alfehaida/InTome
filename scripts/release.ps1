# Cuts a release: bumps the version in Cargo.toml, tags the release commit with its notes, and pushes.
# The tag push triggers .github/workflows/release.yml. Usage: release.ps1 [x.y.z|patch|minor|major] [-DryRun]
param([string]$Version = '', [switch]$DryRun)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
[Console]::OutputEncoding = [Text.Encoding]::UTF8 # git prints UTF-8 subjects
$utf8 = New-Object Text.UTF8Encoding($false)

function Git {
    $out = & git.exe @args
    if ($LASTEXITCODE -ne 0) { throw "git $args failed" }
    $out
}

if ($Version -and $Version -notmatch '^(\d+\.\d+\.\d+|patch|minor|major)$') { throw "Version must be x.y.z, patch, minor or major, not '$Version'." }
if ((Git branch --show-current) -ne 'main') { throw "Releases are cut from main." }
if (Git status --porcelain) { throw "Working tree is not clean; commit or stash first." }
Git fetch origin --tags | Out-Null
if ([int](Git rev-list --count HEAD..origin/main) -ne 0) { throw "main is behind origin/main; pull first." }

$cargoPath = Join-Path $root 'Cargo.toml'
$cargoText = [IO.File]::ReadAllText($cargoPath)
$versionRe = '(?m)^version\s*=\s*"([^"]+)"'
$current = [regex]::Match($cargoText, $versionRe).Groups[1].Value
if ($current -notmatch '^\d+\.\d+\.\d+$') { throw "Cargo.toml version '$current' is not x.y.z." }

# A "Release vX.Y.Z" commit at HEAD means an earlier run stopped before finishing: resume it
# instead of bumping again, and describe from its parent so a tag already on HEAD is not the previous release.
$resume = [regex]::Match((Git log -1 --format=%s), '^Release v(\d+\.\d+\.\d+)$')
$ErrorActionPreference = 'Continue' # Windows PowerShell 5.1 throws on redirected native stderr under 'Stop'
$prevTag = & git.exe describe --tags --abbrev=0 --match 'v*' $(if ($resume.Success) { 'HEAD^' } else { 'HEAD' }) 2>$null
if ($LASTEXITCODE -ne 0) { $prevTag = $null }
$ErrorActionPreference = 'Stop'

if ($Version -match '^\d+\.\d+\.\d+$') { $next = $Version }
elseif ($resume.Success) { $next = $resume.Groups[1].Value }
elseif (-not $prevTag -and -not $Version) { $next = $current }
else {
    $p = [int[]]$current.Split('.')
    switch ($Version) {
        'major' { $p = ($p[0] + 1), 0, 0 }
        'minor' { $p = $p[0], ($p[1] + 1), 0 }
        default { $p[2]++ }
    }
    $next = $p -join '.'
}

$range = if ($prevTag) { "$prevTag..HEAD" } else { 'HEAD' }
# Bullets must not start with '#': git tag strips such lines from the message by default.
$bullets = @(Git log $range --no-merges --format='- %s (%h)' | Where-Object { $_ -notmatch '^- Release v' })
if (-not $bullets) { throw "No commits to release since $prevTag." }
$title = if ($prevTag) { "Changes since ${prevTag}:" } else { 'Changes:' }
$release = [pscustomobject]@{ PrevTag = $prevTag; Version = $next; Notes = (@($title, '') + $bullets) -join "`n" }

Write-Host "Version:      $($release.Version)"
Write-Host "Previous tag: $(if ($release.PrevTag) { $release.PrevTag } else { '(none)' })"
Write-Host $release.Notes
if ($DryRun) { Write-Host "Dry run: nothing changed."; return }

$tag = "v$($release.Version)"
if (Git tag --list $tag) {
    if ((Git rev-parse "$tag^{commit}") -ne (Git rev-parse HEAD)) { throw "Tag $tag exists locally on another commit; if it is stale, delete it with: git tag -d $tag" }
    Write-Host "Tag $tag already points at HEAD; pushing it."
} else {
    if (Git ls-remote --tags origin "refs/tags/$tag") { throw "Tag $tag already exists on origin." }
    if ($current -ne $release.Version) {
        try {
            [IO.File]::WriteAllText($cargoPath, [regex]::new($versionRe).Replace($cargoText, "version = `"$($release.Version)`"", 1), $utf8)
            cargo update --workspace --offline
            if ($LASTEXITCODE -ne 0) { throw "cargo update failed" }
            $lock = [IO.File]::ReadAllText((Join-Path $root 'Cargo.lock'))
            if ($lock -notmatch "(?m)^name = `"intome`"\r?\nversion = `"$([regex]::Escape($release.Version))`"") { throw "cargo update did not set intome to $($release.Version) in Cargo.lock" }
            Git commit -am "Release $tag" | Out-Null
        } catch {
            Git checkout -- Cargo.toml Cargo.lock
            throw
        }
    }
    $notesFile = [IO.Path]::GetTempFileName()
    try {
        [IO.File]::WriteAllText($notesFile, $release.Notes + "`n", $utf8)
        Git tag -a $tag --cleanup=verbatim -F $notesFile
    } finally { Remove-Item $notesFile -Force }
}

Git push --atomic origin main $tag
Write-Host "Pushed $tag"
if ((Git remote get-url origin) -match 'github\.com[:/]([^/]+)/(.+?)(\.git)?$') {
    Write-Host "Watch the release build: https://github.com/$($Matches[1])/$($Matches[2])/actions"
}
