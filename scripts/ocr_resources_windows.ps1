#!/usr/bin/env pwsh
# Windows process-tree sampler for scripts/ocr_resources.py.
#
# Why this file exists, and why it is not a second sampler:
#
# `scripts/ocr_resources.py` owns the phase plan, the summary, and the report.
# Python cannot read per-process threads, handles, private bytes, or CPU time
# on Windows without a new dependency, and this repository pins its
# dependencies. So the platform half stays here, behind one narrow contract.
#
# The caller owns the child process. This script never starts it and never
# stops it. It samples the tree under -RootPid for the plan's total duration,
# then returns. `WindowsSampler.stop` in Python does the cleanup.
#
# The record keys are snake_case and match `RECORD_KEYS` in ocr_resources.py.
# `scripts/measure_ocr_resources.ps1` emits PascalCase keys and writes its own
# report. It is the standalone single-phase tool. This file is the backend.
#
# Exit codes: 0 the plan completed, 1 the root process died mid-plan,
# 2 bad arguments.

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [int]$RootPid,
    [Parameter(Mandatory)]
    [string]$PlanPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Records leave through [Console]::Out, not Write-Output. A function that both
# emits records and returns a value hands the records to the caller's variable
# instead of to stdout. The caller is a Python process that parses stdout.
# The encoding is pinned so a non-ASCII process path survives the decode.
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

if (-not (Test-Path -LiteralPath $PlanPath -PathType Leaf)) {
    [Console]::Error.WriteLine("ocr_resources_windows: no plan at $PlanPath")
    exit 2
}

$plan = Get-Content -LiteralPath $PlanPath -Raw | ConvertFrom-Json
$phases = @($plan.phases)
if ($phases.Count -eq 0) {
    [Console]::Error.WriteLine("ocr_resources_windows: the plan declares no phase")
    exit 2
}
$sampleMilliseconds = [int]$plan.sample_ms
if ($sampleMilliseconds -lt 10) {
    [Console]::Error.WriteLine("ocr_resources_windows: sample_ms must be at least 10")
    exit 2
}

$logical = [double][Environment]::ProcessorCount

# Returns the root and every descendant, parents before children.
function Get-ProcessTreeIds {
    param(
        [uint32]$Root,
        [object[]]$ProcessTable
    )
    $found = New-Object 'System.Collections.Generic.List[uint32]'
    $pending = New-Object 'System.Collections.Generic.Queue[uint32]'
    $found.Add($Root)
    $pending.Enqueue($Root)
    while ($pending.Count -gt 0) {
        $parent = $pending.Dequeue()
        foreach ($item in $ProcessTable) {
            $child = [uint32]$item.ProcessId
            if ([uint32]$item.ParentProcessId -eq $parent -and -not ($found -contains $child)) {
                $found.Add($child)
                $pending.Enqueue($child)
            }
        }
    }
    return @($found)
}

# Moves the physical pointer. A phase without a path leaves it still.
$cursorReady = $false
try {
    Add-Type -AssemblyName System.Windows.Forms
    $cursorReady = $true
} catch {
    [Console]::Error.WriteLine("ocr_resources_windows: pointer control unavailable: $($_.Exception.Message)")
}

function Set-Pointer {
    param([int]$X, [int]$Y)
    if (-not $cursorReady) { return }
    try {
        [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point($X, $Y)
    } catch {
        # A locked or absent desktop refuses the move. Sampling continues.
    }
}

# Emits one JSON line per process, then one `total` line for the tree.
function Write-Sample {
    param(
        [uint32]$Root,
        [string]$Phase,
        [hashtable]$PreviousCpu,
        [object]$PreviousAt
    )
    $now = Get-Date
    $table = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue)
    $ids = @(Get-ProcessTreeIds -Root $Root -ProcessTable $table)
    $rows = @()
    foreach ($processId in $ids) {
        try {
            $process = Get-Process -Id $processId -ErrorAction Stop
            $processInfo = $table | Where-Object { [uint32]$_.ProcessId -eq $processId } |
                Select-Object -First 1
            $cpuSeconds = [double]$process.TotalProcessorTime.TotalSeconds
            $oneCore = $null
            if ($null -ne $PreviousAt -and $PreviousCpu.ContainsKey($processId)) {
                $wallSeconds = ($now - $PreviousAt).TotalSeconds
                if ($wallSeconds -gt 0) {
                    $oneCore = 100.0 * ($cpuSeconds - $PreviousCpu[$processId]) / $wallSeconds
                }
            }
            $PreviousCpu[$processId] = $cpuSeconds
            $cpuPercentOneCore = $null
            $cpuPercent = $null
            if ($null -ne $oneCore) {
                $cpuPercentOneCore = [math]::Round($oneCore, 3)
                $cpuPercent = [math]::Round($oneCore / $logical, 3)
            }
            $rows += [pscustomobject]@{
                timestamp             = $now.ToString("o")
                phase                 = $Phase
                role                  = if ($processId -eq $Root) { "parent" } else { "descendant" }
                pid                   = [int]$processId
                parent_pid            = if ($null -ne $processInfo) { [int][uint32]$processInfo.ParentProcessId } else { 0 }
                process_name          = $process.ProcessName
                executable_path       = if ($null -ne $processInfo) { $processInfo.ExecutablePath } else { $null }
                working_set_bytes     = [int64]$process.WorkingSet64
                working_set_mib       = [math]::Round($process.WorkingSet64 / 1MB, 3)
                private_bytes         = [int64]$process.PrivateMemorySize64
                private_mib           = [math]::Round($process.PrivateMemorySize64 / 1MB, 3)
                cpu_seconds           = [math]::Round($cpuSeconds, 6)
                cpu_percent_one_core  = $cpuPercentOneCore
                cpu_percent           = $cpuPercent
                threads               = @($process.Threads).Count
                handles               = [int64]$process.HandleCount
            }
        } catch {
            continue
        }
    }
    if ($rows.Count -eq 0) {
        return $false
    }
    $working = ($rows | Measure-Object -Property working_set_bytes -Sum).Sum
    $private = ($rows | Measure-Object -Property private_bytes -Sum).Sum
    $cpu = ($rows | Measure-Object -Property cpu_seconds -Sum).Sum
    $threads = ($rows | Measure-Object -Property threads -Sum).Sum
    $handles = ($rows | Measure-Object -Property handles -Sum).Sum
    $oneCoreRates = @($rows | Where-Object { $null -ne $_.cpu_percent_one_core })
    $oneCore = $null
    if ($oneCoreRates.Count -eq $rows.Count) {
        $oneCore = ($oneCoreRates | Measure-Object -Property cpu_percent_one_core -Sum).Sum
    }
    $totalOneCore = $null
    $totalPercent = $null
    if ($null -ne $oneCore) {
        $totalOneCore = [math]::Round($oneCore, 3)
        $totalPercent = [math]::Round($oneCore / $logical, 3)
    }
    foreach ($row in $rows) {
        [Console]::Out.WriteLine(($row | ConvertTo-Json -Compress -Depth 3))
    }
    [Console]::Out.WriteLine(([pscustomobject]@{
        timestamp            = $now.ToString("o")
        phase                = $Phase
        role                 = "total"
        pid                  = 0
        parent_pid           = 0
        process_name         = "process-tree-total"
        executable_path      = $null
        working_set_bytes    = [int64]$working
        working_set_mib      = [math]::Round($working / 1MB, 3)
        private_bytes        = [int64]$private
        private_mib          = [math]::Round($private / 1MB, 3)
        cpu_seconds          = [math]::Round($cpu, 6)
        cpu_percent_one_core = $totalOneCore
        cpu_percent          = $totalPercent
        threads              = [int64]$threads
        handles              = [int64]$handles
    } | ConvertTo-Json -Compress -Depth 3))
    return $true
}

$previousCpu = @{}
$previousAt = $null
$root = [uint32]$RootPid

foreach ($phase in $phases) {
    $label = [string]$phase.label
    $points = @($phase.path)
    $deadline = (Get-Date).AddSeconds([int]$phase.seconds)
    $index = 0
    while ((Get-Date) -lt $deadline) {
        if ($points.Count -gt 0) {
            $point = $points[$index % $points.Count]
            Set-Pointer -X ([int]$point.x) -Y ([int]$point.y)
            $index++
        }
        $alive = $null -ne (Get-Process -Id $RootPid -ErrorAction SilentlyContinue)
        if (-not $alive) {
            [Console]::Error.WriteLine("ocr_resources_windows: root process $RootPid died during phase '$label'")
            exit 1
        }
        $emitted = Write-Sample -Root $root -Phase $label -PreviousCpu $previousCpu -PreviousAt $previousAt
        if (-not $emitted) {
            [Console]::Error.WriteLine("ocr_resources_windows: root process $RootPid died during phase '$label'")
            exit 1
        }
        $previousAt = Get-Date
        if ((Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds $sampleMilliseconds
        }
    }
}

exit 0
