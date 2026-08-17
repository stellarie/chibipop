# Refreshes the copy at Documents\chibipop-latest.
#
# Two modes, chosen by what is already there:
#
#   empty folder  seed it, the way a downloaded zip would: exe, deconjugator,
#                 README, LICENSE. No config and no database, so the first
#                 launch takes the first-run path.
#   an install    replace chibipop.exe only. chibipop.toml, library/ and
#                 data/ are the user's own and are never touched.
#
# This script deletes nothing. Run it after every release build.
#
#   pwsh -File scripts/blank-copy.ps1
#
# See docs/REFERENCE.md.

param(
    [string]$Destination = (Join-Path $HOME 'Documents\chibipop-latest')
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\chibipop.exe'
$target = Join-Path $Destination 'chibipop.exe'

if (-not (Test-Path $exe)) {
    throw "no release build at $exe - run: cargo build --release"
}

# Windows will not overwrite a running executable.
$running = Get-Process chibipop -ErrorAction SilentlyContinue |
           Where-Object { $_.Path -eq $target }
if ($running) {
    throw "chibipop is running from $Destination (pid $($running[0].Id)). Quit it first."
}

$seeding = -not (Test-Path $target)

if ($seeding) {
    New-Item -ItemType Directory -Path (Join-Path $Destination 'data') -Force | Out-Null
    Copy-Item (Join-Path $repo 'data\deconjugator.json') (Join-Path $Destination 'data')
    Copy-Item (Join-Path $repo 'README.md') $Destination
    Copy-Item (Join-Path $repo 'LICENSE')   $Destination
}

Copy-Item $exe $Destination -Force

$version = (& $target --version) -replace '^chibipop\s+', ''
Write-Output $(if ($seeding) { "seeded blank: $Destination" } else { "exe refreshed: $Destination" })
Write-Output "version:      $version"

# Say what survived, so a refresh is never a silent surprise.
if (-not $seeding) {
    $kept = @()
    if (Test-Path (Join-Path $Destination 'chibipop.toml')) { $kept += 'chibipop.toml' }
    $lib = Get-ChildItem (Join-Path $Destination 'library') -Filter '*.zip' -ErrorAction SilentlyContinue
    if ($lib) { $kept += "library/ ($($lib.Count) archives)" }
    $db = Get-ChildItem (Join-Path $Destination 'data') -Filter '*.sqlite' -ErrorAction SilentlyContinue
    foreach ($f in $db) { $kept += ("{0} ({1:N0} MB)" -f $f.Name, ($f.Length / 1MB)) }
    Write-Output $(if ($kept) { "kept:         " + ($kept -join ', ') } else { 'kept:         nothing else was there' })
}
