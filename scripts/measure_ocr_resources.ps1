[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$FilePath,
    [string[]]$ArgumentList = @(),
    [int]$DurationSeconds = 60,
    [int]$SampleMilliseconds = 100,
    [string]$Label = "ocr",
    [string]$OutputPath,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($DurationSeconds -lt 1) {
    throw "DurationSeconds must be at least 1."
}
if ($SampleMilliseconds -lt 10) {
    throw "SampleMilliseconds must be at least 10."
}

$plan = [ordered]@{
    file = $FilePath
    arguments = $ArgumentList
    duration_seconds = $DurationSeconds
    sample_milliseconds = $SampleMilliseconds
    label = $Label
    logical_processors = [Environment]::ProcessorCount
}

if ($DryRun) {
    $plan | ConvertTo-Json -Depth 4
    exit 0
}

if (-not $OutputPath) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutputPath = Join-Path (Get-Location) "ocr-resources-$stamp.json"
}

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

$logical = [double][Environment]::ProcessorCount
$previousCpu = @{}
$previousAt = $null
$records = @()
$stoppedIds = @()
$remainingIds = @()
$started = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -PassThru
$rootPid = [uint32]$started.Id
$stopAt = (Get-Date).AddSeconds($DurationSeconds)

try {
    do {
        $now = Get-Date
        $table = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue)
        $ids = @(Get-ProcessTreeIds -Root $rootPid -ProcessTable $table)
        $rows = @()
        foreach ($processId in $ids) {
            try {
                $process = Get-Process -Id $processId -ErrorAction Stop
                $processInfo = $table | Where-Object { [uint32]$_.ProcessId -eq $processId } |
                    Select-Object -First 1
                $cpuSeconds = [double]$process.TotalProcessorTime.TotalSeconds
                $oneCore = $null
                if ($null -ne $previousAt -and $previousCpu.ContainsKey($processId)) {
                    $wallSeconds = ($now - $previousAt).TotalSeconds
                    if ($wallSeconds -gt 0) {
                        $oneCore = 100.0 * ($cpuSeconds - $previousCpu[$processId]) / $wallSeconds
                    }
                }
                    $previousCpu[$processId] = $cpuSeconds
                $cpuPercentOneCore = $null
                $cpuPercent = $null
                if ($null -ne $oneCore) {
                    $cpuPercentOneCore = [math]::Round($oneCore, 3)
                    $cpuPercent = [math]::Round($oneCore / $logical, 3)
                }
                $rows += [pscustomobject]@{
                    Timestamp = $now.ToString("o")
                    Phase = $Label
                    Role = if ($processId -eq $rootPid) { "parent" } else { "descendant" }
                    Pid = $processId
                    ParentPid = if ($null -ne $processInfo) { [uint32]$processInfo.ParentProcessId } else { 0 }
                    ProcessName = $process.ProcessName
                    ExecutablePath = if ($null -ne $processInfo) { $processInfo.ExecutablePath } else { $null }
                    WorkingSetBytes = [int64]$process.WorkingSet64
                    WorkingSetMiB = [math]::Round($process.WorkingSet64 / 1MB, 3)
                    PrivateBytes = [int64]$process.PrivateMemorySize64
                    PrivateMiB = [math]::Round($process.PrivateMemorySize64 / 1MB, 3)
                    CpuSeconds = [math]::Round($cpuSeconds, 6)
                    CpuPercentOneCore = $cpuPercentOneCore
                    CpuPercent = $cpuPercent
                    Threads = @($process.Threads).Count
                    Handles = [int64]$process.HandleCount
                }
            } catch {
                continue
            }
        }
        if ($rows.Count -gt 0) {
            $working = ($rows | Measure-Object -Property WorkingSetBytes -Sum).Sum
            $private = ($rows | Measure-Object -Property PrivateBytes -Sum).Sum
            $cpu = ($rows | Measure-Object -Property CpuSeconds -Sum).Sum
            $threads = ($rows | Measure-Object -Property Threads -Sum).Sum
            $handles = ($rows | Measure-Object -Property Handles -Sum).Sum
            $oneCoreRates = @($rows | Where-Object { $null -ne $_.CpuPercentOneCore })
            $oneCore = $null
            if ($oneCoreRates.Count -eq $rows.Count) {
                $oneCore = ($oneCoreRates | Measure-Object -Property CpuPercentOneCore -Sum).Sum
            }
            $totalCpuPercentOneCore = $null
            $totalCpuPercent = $null
            if ($null -ne $oneCore) {
                $totalCpuPercentOneCore = [math]::Round($oneCore, 3)
                $totalCpuPercent = [math]::Round($oneCore / $logical, 3)
            }
            foreach ($row in $rows) {
                $records += $row
            }
            $records += [pscustomobject]@{
                Timestamp = $now.ToString("o")
                Phase = $Label
                Role = "total"
                Pid = 0
                ParentPid = 0
                ProcessName = "process-tree-total"
                ExecutablePath = $null
                WorkingSetBytes = [int64]$working
                WorkingSetMiB = [math]::Round($working / 1MB, 3)
                PrivateBytes = [int64]$private
                PrivateMiB = [math]::Round($private / 1MB, 3)
                CpuSeconds = [math]::Round($cpu, 6)
                CpuPercentOneCore = $totalCpuPercentOneCore
                CpuPercent = $totalCpuPercent
                Threads = [int64]$threads
                Handles = [int64]$handles
            }
        }
        $previousAt = $now
        $alive = $false
        try {
            $alive = -not (Get-Process -Id $rootPid -ErrorAction Stop).HasExited
        } catch {
            $alive = $false
        }
        if ($alive -and (Get-Date) -lt $stopAt) {
            Start-Sleep -Milliseconds $SampleMilliseconds
        }
    } while ($alive -and (Get-Date) -lt $stopAt)
} finally {
    $table = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue)
    [uint32[]]$cleanupIds = @(Get-ProcessTreeIds -Root $rootPid -ProcessTable $table)
    [array]::Reverse($cleanupIds)
    foreach ($processId in $cleanupIds) {
        try {
            Stop-Process -Id $processId -Force -ErrorAction Stop
            $stoppedIds += $processId
        } catch {
            continue
        }
    }
    Start-Sleep -Milliseconds 100
    foreach ($processId in $cleanupIds) {
        if (Get-Process -Id $processId -ErrorAction SilentlyContinue) {
            $remainingIds += $processId
        }
    }
    $report = [ordered]@{
        schema = "chibipop-ocr-resources/v1"
        started_at = $started.StartTime.ToString("o")
        sampled_until = (Get-Date).ToString("o")
        root_pid = $rootPid
        command = $plan
        cleanup_stopped_process_ids = @($stoppedIds)
        cleanup_remaining_process_ids = @($remainingIds)
        records = @($records)
    }
    $report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $OutputPath -Encoding utf8
}

Write-Output "Wrote $OutputPath"
