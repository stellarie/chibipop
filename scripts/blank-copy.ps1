# Refreshes the copy at Documents\chibipop-latest.
#
# Two modes, chosen by what is already there:
#
#   empty folder  seed it, the way a downloaded zip would: exe, deconjugator,
#                 README, LICENSE, plugins/. No config and no database, so
#                 the first launch takes the first-run path.
#   an install    replace chibipop.exe, plus each plugin's plugin.toml and
#                 adapter.py. chibipop.toml, library/, data/ and each
#                 plugin's config.toml are the user's own and are never
#                 touched if already present.
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

# plugins/: on seed, copy the whole tree, template configs and all. On
# refresh, plugin.toml and adapter.py are code and always refresh; each
# plugin's config.toml is the user's own and is kept if already present.
$repoPlugins = Join-Path $repo 'plugins'
$destPlugins = Join-Path $Destination 'plugins'
$pluginNotes = @()

if ($seeding) {
    if (Test-Path $repoPlugins) {
        New-Item -ItemType Directory -Path $destPlugins -Force | Out-Null
        Copy-Item (Join-Path $repoPlugins '*') $destPlugins -Recurse -Force
    }
} elseif (Test-Path $repoPlugins) {
    Get-ChildItem $repoPlugins -Directory |
        Where-Object { Test-Path (Join-Path $_.FullName 'plugin.toml') } |
        ForEach-Object {
            $name = $_.Name
            $srcDir = $_.FullName
            $dstDir = Join-Path $destPlugins $name
            New-Item -ItemType Directory -Path $dstDir -Force | Out-Null

            foreach ($file in 'plugin.toml', 'adapter.py') {
                $srcFile = Join-Path $srcDir $file
                if (Test-Path $srcFile) {
                    Copy-Item $srcFile $dstDir -Force
                }
            }

            $dstConfig = Join-Path $dstDir 'config.toml'
            if (Test-Path $dstConfig) {
                $pluginNotes += "plugins/$name (config.toml kept)"
            } else {
                $srcConfig = Join-Path $srcDir 'config.toml'
                if (Test-Path $srcConfig) {
                    Copy-Item $srcConfig $dstConfig
                }
                $pluginNotes += "plugins/$name (config.toml seeded)"
            }
        }
}

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
    $kept += $pluginNotes
    Write-Output $(if ($kept) { "kept:         " + ($kept -join ', ') } else { 'kept:         nothing else was there' })
}
