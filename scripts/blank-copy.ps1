# Refreshes the blank copy at Documents\chibipop-latest.
#
# Blank means what a downloaded zip contains: the executable, the
# deconjugator, and the two text files. No chibipop.toml and no database, so
# the next launch takes the first-run path.
#
#   pwsh -File scripts/blank-copy.ps1
#   pwsh -File scripts/blank-copy.ps1 -Force        # wipe a database too
#
# Run it after every release build. See docs/REFERENCE.md.

param(
    [string]$Destination = (Join-Path $HOME 'Documents\chibipop-latest'),
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\chibipop.exe'

if (-not (Test-Path $exe)) {
    throw "no release build at $exe - run: cargo build --release"
}

# A database is 200-900 MB of the user's own build. Never delete one silently.
$db = Get-ChildItem -Path (Join-Path $Destination 'data') -Filter '*.sqlite' -ErrorAction SilentlyContinue
if ($db -and -not $Force) {
    throw "$Destination holds a database ($($db[0].Name)). Move it, or pass -Force."
}

if (Test-Path $Destination) { Remove-Item $Destination -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $Destination 'data') -Force | Out-Null

Copy-Item $exe                                    $Destination
Copy-Item (Join-Path $repo 'data\deconjugator.json') (Join-Path $Destination 'data')
Copy-Item (Join-Path $repo 'README.md')           $Destination
Copy-Item (Join-Path $repo 'LICENSE')             $Destination

$version = (& (Join-Path $Destination 'chibipop.exe') --version) -replace '^chibipop\s+', ''
Write-Output "blank copy: $Destination"
Write-Output "version:    $version"
Get-ChildItem $Destination -Recurse -File |
    ForEach-Object { "  {0,-24} {1,10:N0} bytes" -f $_.Name, $_.Length }
