# Finds what the NVDA scripts need on any collaborator's machine, so nobody
# edits paths. Dot-source it: . (Join-Path $PSScriptRoot 'nvda_locate.ps1')
#
#   Find-PrismRoot      $env:PRISM_ROOT, else the folder next to this repo that
#                       holds Prism's source (prism\__main__.py), whatever it is named.
#   Find-NvdaClientDll  $env:NVDA_CLIENT_DLL, else nvdaControllerClient64.dll from
#                       the accessible_output2 package in Prism's .venv, in any
#                       site-packages of `python` or `py` (user site included),
#                       or in an installed Prism.

function Find-PrismRoot {
    if ($env:PRISM_ROOT) { return $env:PRISM_ROOT }
    $parent = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
    Get-ChildItem -LiteralPath $parent -Directory -ErrorAction SilentlyContinue |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'prism\__main__.py') } |
        Select-Object -First 1 -ExpandProperty FullName
}

function Find-NvdaClientDll {
    if ($env:NVDA_CLIENT_DLL) {
        if (Test-Path -LiteralPath $env:NVDA_CLIENT_DLL) { return $env:NVDA_CLIENT_DLL }
        throw "NVDA_CLIENT_DLL points to a file that does not exist: $env:NVDA_CLIENT_DLL"
    }
    $sites = @()
    $prism = Find-PrismRoot
    if ($prism) { $sites += Join-Path $prism '.venv\Lib\site-packages' }
    foreach ($py in 'python', 'py') {
        if (-not (Get-Command $py -ErrorAction SilentlyContinue)) { continue }
        # Asks Python where packages live without importing any of them. -P keeps
        # the current folder off sys.path; an interpreter older than 3.11 just fails.
        $sites += & $py -P -c "import site; print('\n'.join(site.getsitepackages() + [site.getusersitepackages()]))" 2>$null
    }
    $sites += "${env:ProgramFiles(x86)}\Prism\_internal", "$env:ProgramFiles\Prism\_internal"
    foreach ($site in $sites | Where-Object { $_ }) {
        $dll = Join-Path $site 'accessible_output2\lib\nvdaControllerClient64.dll'
        if (Test-Path -LiteralPath $dll) { return $dll }
    }
    throw "nvdaControllerClient64.dll not found. Install it with 'python -m pip install accessible_output2', or set NVDA_CLIENT_DLL to its full path."
}
