[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$FilePath,
    [string[]]$ArgumentList = @(),
    [int]$DurationSeconds = 60,
    [int]$SampleMilliseconds = 100,
    [string]$Label = "ocr",
    [string]$OutputPath,
    [string]$PhaseFile,
    [string]$BackendIdentityFile,
    [string]$RequiredPhases = "cold-startup,post-handshake-idle,first-recognition,repeated-identical-recognition,uncached-recognition,post-activity-steady-state",
    [string]$BackendId = "unknown",
    [string]$BackendVersion = "unknown",
    [string]$RunnerImage = "",
    [string]$RunnerOs = "",
    [string]$RunnerArchitecture = "",
    [string]$BuildRevision = "",
    [string]$FixtureSha256 = "",
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

function Get-RedactedValue {
    param([AllowNull()][string]$Value)

    if ([string]::IsNullOrEmpty($Value)) {
        return $Value
    }
    $trimmed = $Value.Trim('"')
    if ($trimmed -match '^[A-Za-z]:[\\/]' -or $trimmed -match '^\\\\') {
        $leaf = Split-Path -Leaf ($trimmed -replace '[\\/]+$', '')
        if ($leaf) {
            return "<path>/$leaf"
        }
        return "<path>"
    }
    return [regex]::Replace($Value, '(?i)(?:[A-Za-z]:[\\/]|\\\\)[^\s"'']+', '<path>')
}

function Get-UnixMilliseconds {
    param([datetime]$At)
    return ([DateTimeOffset]$At.ToUniversalTime()).ToUnixTimeMilliseconds()
}

function Get-PhaseEvents {
    $events = New-Object 'System.Collections.Generic.List[object]'
    if ([string]::IsNullOrEmpty($PhaseFile)) {
        return @($events | ForEach-Object { $_ })
    }
    try {
        if (-not (Test-Path -LiteralPath $PhaseFile)) {
            return @($events | ForEach-Object { $_ })
        }
        foreach ($line in @(Get-Content -LiteralPath $PhaseFile -ErrorAction Stop)) {
            $parts = $line -split "`t", 3
            if ($parts.Count -ne 3) {
                continue
            }
            $at = [int64]0
            if (-not [int64]::TryParse($parts[1], [ref]$at)) {
                continue
            }
            if ($parts[0] -notin @("start", "end") -or [string]::IsNullOrWhiteSpace($parts[2])) {
                continue
            }
            $events.Add([pscustomobject]@{
                    event = $parts[0]
                    unix_milliseconds = $at
                    name = $parts[2]
                })
        }
    } catch {
        return @($events | ForEach-Object { $_ })
    }
    return @($events | ForEach-Object { $_ })
}

function Get-PhaseNameAt {
    param(
        [datetime]$At,
        [object[]]$Events
    )
    if ([string]::IsNullOrEmpty($PhaseFile)) {
        return $Label
    }
    $atUnix = Get-UnixMilliseconds $At
    $validEvents = @($Events | Where-Object {
            $null -ne $_ -and $null -ne $_.PSObject.Properties["event"]
        })
    $open = @{}
    $intervals = New-Object 'System.Collections.Generic.List[object]'
    foreach ($event in @($validEvents | Sort-Object -Property unix_milliseconds)) {
        if ($event.event -eq "start") {
            $open[$event.name] = [int64]$event.unix_milliseconds
        } elseif ($open.ContainsKey($event.name)) {
            $intervals.Add([pscustomobject]@{
                    name = $event.name
                    start = $open[$event.name]
                    end = [int64]$event.unix_milliseconds
                })
            $open.Remove($event.name)
        }
    }
    foreach ($name in $open.Keys) {
        $intervals.Add([pscustomobject]@{
                name = $name
                start = $open[$name]
                end = $atUnix
            })
    }
    $match = @($intervals | Where-Object { $_.start -le $atUnix -and $_.end -ge $atUnix } |
        Sort-Object -Property start | Select-Object -Last 1)
    if ($match.Count -gt 0) {
        return [string]$match[0].name
    }
    return "unknown"
}

function Get-BackendIdentity {
    if ([string]::IsNullOrEmpty($BackendIdentityFile)) {
        return $null
    }
    try {
        if (Test-Path -LiteralPath $BackendIdentityFile) {
            return Get-Content -LiteralPath $BackendIdentityFile -Raw -ErrorAction Stop | ConvertFrom-Json
        }
    } catch {
        return $null
    }
    return $null
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
    return @($found | ForEach-Object { $_ })
}

function Get-ProcessStartTicks {
    param([System.Diagnostics.Process]$Process)
    try {
        return [int64]$Process.StartTime.ToUniversalTime().Ticks
    } catch {
        return $null
    }
}

function Get-ChildExitCode {
    param([System.Diagnostics.Process]$Process)
    try {
        $Process.Refresh()
        if ($Process.HasExited) {
            return [int]$Process.ExitCode
        }
    } catch {
        return $null
    }
    return $null
}

function Get-Percentile {
    param(
        [double[]]$Values,
        [double]$Fraction
    )
    if ($Values.Count -eq 0) {
        return $null
    }
    $sorted = @($Values | Sort-Object)
    $rank = [math]::Ceiling($Fraction * $sorted.Count)
    $index = [math]::Max(0, [math]::Min($sorted.Count - 1, $rank - 1))
    return [double]$sorted[$index]
}

function Get-Stats {
    param(
        [object[]]$Items,
        [string]$Property,
        [switch]$Bytes
    )
    $values = @($Items | ForEach-Object {
            $value = $_.$Property
            if ($null -ne $value) {
                [double]$value
            }
        })
    if ($values.Count -eq 0) {
        return $null
    }
    $median = Get-Percentile -Values $values -Fraction 0.5
    $p95 = Get-Percentile -Values $values -Fraction 0.95
    $peak = [double]($values | Measure-Object -Maximum).Maximum
    $result = [ordered]@{
        count = $values.Count
        median = [math]::Round($median, 3)
        p95 = [math]::Round($p95, 3)
        peak = [math]::Round($peak, 3)
    }
    if ($Bytes) {
        $result.median_mib = [math]::Round($median / 1MB, 3)
        $result.p95_mib = [math]::Round($p95 / 1MB, 3)
        $result.peak_mib = [math]::Round($peak / 1MB, 3)
    }
    return [pscustomobject]$result
}

function Get-ResourceAggregates {
    param([object[]]$Items)

    $output = New-Object 'System.Collections.Generic.List[object]'
    foreach ($phaseGroup in @($Items | Group-Object -Property Phase)) {
        foreach ($role in @("parent", "descendant", "total")) {
            $roleItems = @($phaseGroup.Group | Where-Object { $_.Role -eq $role })
            if ($roleItems.Count -eq 0) {
                continue
            }
            $output.Add([pscustomobject]@{
                    phase = $phaseGroup.Name
                    role = $role
                    sample_count = $roleItems.Count
                    working_set_bytes = Get-Stats -Items $roleItems -Property WorkingSetBytes -Bytes
                    private_bytes = Get-Stats -Items $roleItems -Property PrivateBytes -Bytes
                    cpu_seconds = Get-Stats -Items $roleItems -Property CpuSeconds
                    cpu_percent = Get-Stats -Items $roleItems -Property CpuPercent
                    threads = Get-Stats -Items $roleItems -Property Threads
                    handles = Get-Stats -Items $roleItems -Property Handles
                })
        }
    }
    return @($output | ForEach-Object { $_ })
}

function Get-PhaseCoverage {
    param(
        [object[]]$Events,
        [object[]]$Records
    )
    $required = @($RequiredPhases -split ',' | ForEach-Object { $_.Trim() } |
        Where-Object { $_ })
    $validEvents = @($Events | Where-Object {
            $null -ne $_ -and $null -ne $_.PSObject.Properties["event"]
        })
    $applicable = $validEvents.Count -gt 0 -and $required.Count -gt 0
    $observed = @($Records | Where-Object { $_.Role -eq "total" } |
        Select-Object -ExpandProperty Phase -Unique)
    $missing = @()
    if ($applicable) {
        $missing = @($required | Where-Object { $_ -notin $observed })
    }
    [ordered]@{
        applicable = $applicable
        required = $required
        observed = $observed
        missing = $missing
        complete = @($missing).Count -eq 0
    }
}

$plan = [ordered]@{
    file = Get-RedactedValue $FilePath
    arguments = @($ArgumentList | ForEach-Object { Get-RedactedValue $_ })
    duration_seconds = $DurationSeconds
    sample_milliseconds = $SampleMilliseconds
    label = $Label
    logical_processors = [Environment]::ProcessorCount
}
$effectiveRunnerImage = $RunnerImage
if (-not $effectiveRunnerImage) { $effectiveRunnerImage = "local" }
$effectiveRunnerOs = $RunnerOs
if (-not $effectiveRunnerOs) { $effectiveRunnerOs = [Environment]::OSVersion.Platform.ToString() }
$effectiveRunnerArchitecture = $RunnerArchitecture
if (-not $effectiveRunnerArchitecture) {
    if ([Environment]::Is64BitOperatingSystem) { $effectiveRunnerArchitecture = "x64" }
    else { $effectiveRunnerArchitecture = "x86" }
}
$safeBuildRevision = $null
if ($BuildRevision -match '^[0-9a-fA-F]{7,64}$') {
    $safeBuildRevision = $BuildRevision
}

if ($DryRun) {
    [ordered]@{
        schema = "chibipop-ocr-resources/v1"
        command = $plan
        runner = [ordered]@{
            image = $effectiveRunnerImage
            os = $effectiveRunnerOs
            architecture = $effectiveRunnerArchitecture
            build_revision = $safeBuildRevision
        }
        backend = [ordered]@{ id = $BackendId; version = $BackendVersion }
        fixture_sha256 = $FixtureSha256
        required_phases = @($RequiredPhases -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    } | ConvertTo-Json -Depth 8
    exit 0
}

if (-not $OutputPath) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutputPath = Join-Path (Get-Location) "ocr-resources-$stamp.json"
}

$logical = [double][Environment]::ProcessorCount
$previousCpu = @{}
$previousAt = $null
$records = New-Object 'System.Collections.Generic.List[object]'
$observedIds = New-Object 'System.Collections.Generic.HashSet[uint32]'
$processIdentities = @{}
$identityMismatches = New-Object 'System.Collections.Generic.HashSet[uint32]'
$identityUnverified = New-Object 'System.Collections.Generic.HashSet[uint32]'
$stoppedIds = New-Object 'System.Collections.Generic.List[uint32]'
$remainingIds = New-Object 'System.Collections.Generic.HashSet[uint32]'
$started = $null
$startedAt = Get-Date
$rootPid = [uint32]0
$stopAt = $null
$alive = $false
$timedOut = $false
$childExited = $false
$childExitCode = $null
$launchError = $null

try {
    try {
        $started = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -PassThru
        $rootPid = [uint32]$started.Id
        $observedIds.Add($rootPid) | Out-Null
        $startedAt = Get-Date
        $stopAt = $startedAt.AddSeconds($DurationSeconds)
    } catch {
        $launchError = $_.Exception.Message
    }
    if ($null -ne $started) {
        do {
            $now = Get-Date
            $phaseEvents = Get-PhaseEvents
            $backendIdentity = Get-BackendIdentity
            $table = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue)
            $ids = @(Get-ProcessTreeIds -Root $rootPid -ProcessTable $table)
            foreach ($processId in $ids) {
                $observedIds.Add($processId) | Out-Null
            }
            $rows = New-Object 'System.Collections.Generic.List[object]'
            foreach ($processId in $ids) {
                try {
                    $process = Get-Process -Id $processId -ErrorAction Stop
                    $processInfo = $table | Where-Object { [uint32]$_.ProcessId -eq $processId } |
                        Select-Object -First 1
                    $startTicks = Get-ProcessStartTicks $process
                    $identityKey = [string]$processId
                    $known = $processIdentities[$identityKey]
                    if ($null -ne $known) {
                        if ($null -ne $known.StartTimeUtcTicks -and $null -ne $startTicks -and
                            $known.StartTimeUtcTicks -ne $startTicks) {
                            $identityMismatches.Add($processId) | Out-Null
                            continue
                        }
                        if ($null -eq $known.StartTimeUtcTicks -and $null -ne $startTicks) {
                            $known = [pscustomobject]@{
                                StartTimeUtcTicks = $startTicks
                                ProcessName = $known.ProcessName
                                ExecutablePath = $known.ExecutablePath
                            }
                            $processIdentities[$identityKey] = $known
                        }
                    } else {
                        $known = [pscustomobject]@{
                            StartTimeUtcTicks = $startTicks
                            ProcessName = $process.ProcessName
                            ExecutablePath = if ($null -ne $processInfo) { Get-RedactedValue $processInfo.ExecutablePath } else { $null }
                        }
                        $processIdentities[$identityKey] = $known
                    }
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
                    $backendVersionForRow = $BackendVersion
                    $backendIdForRow = $BackendId
                    $languageForRow = $null
                    $threadSettingsForRow = $null
                    $scalesForRow = $null
                    $modelHashesForRow = $null
                    $pluginHashesForRow = $null
                    if ($null -ne $backendIdentity) {
                        if ($backendIdentity.id) { $backendIdForRow = [string]$backendIdentity.id }
                        if ($backendIdentity.version) { $backendVersionForRow = [string]$backendIdentity.version }
                        if ($backendIdentity.language) { $languageForRow = [string]$backendIdentity.language }
                        if ($null -ne $backendIdentity.thread_settings) {
                            $threadSettingsForRow = $backendIdentity.thread_settings | ConvertTo-Json -Compress
                        }
                        if ($null -ne $backendIdentity.scales) {
                            $scalesForRow = (@($backendIdentity.scales) -join ",")
                        }
                        if ($null -ne $backendIdentity.model_hashes) {
                            $modelHashesForRow = $backendIdentity.model_hashes | ConvertTo-Json -Compress
                        }
                        if ($null -ne $backendIdentity.plugin_hashes) {
                            $pluginHashesForRow = $backendIdentity.plugin_hashes | ConvertTo-Json -Compress
                        }
                    }
                    $rows.Add([pscustomobject]@{
                            Timestamp = $now.ToString("o")
                            Phase = Get-PhaseNameAt -At $now -Events $phaseEvents
                            BackendId = $backendIdForRow
                            BackendVersion = $backendVersionForRow
                            Language = $languageForRow
                            ThreadSettings = $threadSettingsForRow
                            Scales = $scalesForRow
                            FixtureSha256 = $FixtureSha256
                            ModelHashes = $modelHashesForRow
                            PluginHashes = $pluginHashesForRow
                            RunnerImage = $effectiveRunnerImage
                            Role = if ($processId -eq $rootPid) { "parent" } else { "descendant" }
                            Pid = $processId
                            ParentPid = if ($null -ne $processInfo) { [uint32]$processInfo.ParentProcessId } else { 0 }
                            ProcessName = $process.ProcessName
                            ProcessStartTimeUtcTicks = $startTicks
                            ExecutablePath = if ($null -ne $processInfo) { Get-RedactedValue $processInfo.ExecutablePath } else { $null }
                            WorkingSetBytes = [int64]$process.WorkingSet64
                            WorkingSetMiB = [math]::Round($process.WorkingSet64 / 1MB, 3)
                            PrivateBytes = [int64]$process.PrivateMemorySize64
                            PrivateMiB = [math]::Round($process.PrivateMemorySize64 / 1MB, 3)
                            CpuSeconds = [math]::Round($cpuSeconds, 6)
                            CpuPercentOneCore = $cpuPercentOneCore
                            CpuPercent = $cpuPercent
                            Threads = @($process.Threads).Count
                            Handles = [int64]$process.HandleCount
                        })
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
                    $records.Add($row)
                }
                $records.Add([pscustomobject]@{
                        Timestamp = $now.ToString("o")
                        Phase = Get-PhaseNameAt -At $now -Events $phaseEvents
                        BackendId = $backendIdForRow
                        BackendVersion = $backendVersionForRow
                        Language = $languageForRow
                        ThreadSettings = $threadSettingsForRow
                        Scales = $scalesForRow
                        FixtureSha256 = $FixtureSha256
                        ModelHashes = $modelHashesForRow
                        PluginHashes = $pluginHashesForRow
                        RunnerImage = $effectiveRunnerImage
                        Role = "total"
                        Pid = 0
                        ParentPid = 0
                        ProcessName = "process-tree-total"
                        ProcessStartTimeUtcTicks = $null
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
                    })
            }
            $previousAt = $now
            try {
                $started.Refresh()
                $alive = -not $started.HasExited
            } catch {
                $alive = $false
            }
            if (-not $alive) {
                $childExited = $true
                $childExitCode = Get-ChildExitCode $started
                break
            }
            if ((Get-Date) -ge $stopAt) {
                $timedOut = $true
                break
            }
            Start-Sleep -Milliseconds $SampleMilliseconds
        } while ($alive)
    }
} finally {
    if ($null -ne $started) {
        $table = @(Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue)
        foreach ($processId in @(Get-ProcessTreeIds -Root $rootPid -ProcessTable $table)) {
            $observedIds.Add($processId) | Out-Null
        }
        foreach ($processId in @($observedIds | Sort-Object -Descending)) {
            $identityKey = [string]$processId
            $known = $processIdentities[$identityKey]
            try {
                $current = Get-Process -Id $processId -ErrorAction Stop
            } catch {
                continue
            }
            if ($null -eq $known -or $null -eq $known.StartTimeUtcTicks) {
                $identityUnverified.Add($processId) | Out-Null
                continue
            }
            $currentTicks = Get-ProcessStartTicks $current
            if ($null -eq $currentTicks) {
                $identityUnverified.Add($processId) | Out-Null
                continue
            }
            if ($currentTicks -ne $known.StartTimeUtcTicks) {
                $identityMismatches.Add($processId) | Out-Null
                continue
            }
            try {
                Stop-Process -InputObject $current -Force -ErrorAction Stop
                $stoppedIds.Add($processId)
            } catch {
                continue
            }
        }
        Start-Sleep -Milliseconds 100
        foreach ($processId in @($observedIds)) {
            try {
                $current = Get-Process -Id $processId -ErrorAction Stop
            } catch {
                continue
            }
            $known = $processIdentities[[string]$processId]
            if ($null -eq $known -or $null -eq $known.StartTimeUtcTicks) {
                $identityUnverified.Add($processId) | Out-Null
                continue
            }
            $currentTicks = Get-ProcessStartTicks $current
            if ($null -eq $currentTicks) {
                $identityUnverified.Add($processId) | Out-Null
            } elseif ($currentTicks -ne $known.StartTimeUtcTicks) {
                $identityMismatches.Add($processId) | Out-Null
            } else {
                $remainingIds.Add($processId) | Out-Null
            }
        }
    }
    $failureCategories = New-Object 'System.Collections.Generic.List[string]'
    if ($null -ne $launchError) {
        $failureCategories.Add("launch-error")
    }
    if ($timedOut) {
        $failureCategories.Add("timeout")
    }
    if ($null -ne $childExitCode -and $childExitCode -ne 0) {
        $failureCategories.Add("child-failure")
    }
    if ($remainingIds.Count -gt 0) {
        $failureCategories.Add("cleanup-survivor")
    }
    if ($identityUnverified.Count -gt 0) {
        $failureCategories.Add("cleanup-identity-unverified")
    }
    if ($identityMismatches.Count -gt 0) {
        $failureCategories.Add("cleanup-identity-mismatch")
    }
    $phaseEvents = Get-PhaseEvents
    $finalBackendIdentity = Get-BackendIdentity
    if ($null -eq $finalBackendIdentity) {
        $finalBackendIdentity = [ordered]@{ id = $BackendId; version = $BackendVersion }
    }
    $backendStatusProperty = $finalBackendIdentity.PSObject.Properties["status"]
    $backendStatus = if ($null -ne $backendStatusProperty) { $backendStatusProperty.Value } else { $null }
    $phaseCoverage = Get-PhaseCoverage -Events $phaseEvents -Records @($records | ForEach-Object { $_ })
    if ($backendStatus -eq "unavailable") {
        $phaseCoverage.applicable = $false
        $phaseCoverage.missing = @()
        $phaseCoverage.complete = $true
    } elseif (-not $phaseCoverage.complete) {
        $failureCategories.Add("phase-resource-incomplete")
    }
    $report = [ordered]@{
        schema = "chibipop-ocr-resources/v1"
        started_at = $startedAt.ToString("o")
        sampled_until = (Get-Date).ToString("o")
        root_pid = $rootPid
        child_exit_code = $childExitCode
        child_exited = $childExited
        timed_out = $timedOut
        timeout_seconds = $DurationSeconds
        failure_categories = @($failureCategories | Select-Object -Unique)
        runner = [ordered]@{
            image = $effectiveRunnerImage
            os = $effectiveRunnerOs
            architecture = $effectiveRunnerArchitecture
            build_revision = $safeBuildRevision
        }
        backend = $finalBackendIdentity
        fixture_sha256 = $FixtureSha256
        command = $plan
        cleanup_stopped_process_ids = @($stoppedIds | ForEach-Object { [uint32]$_ })
        cleanup_remaining_process_ids = @($remainingIds | ForEach-Object { [uint32]$_ })
        cleanup_identity_mismatch_process_ids = @($identityMismatches | ForEach-Object { [uint32]$_ })
        cleanup_identity_unverified_process_ids = @($identityUnverified | ForEach-Object { [uint32]$_ })
        phase_events = @($phaseEvents | ForEach-Object { $_ })
        phase_coverage = $phaseCoverage
        records = @($records | ForEach-Object { $_ })
        aggregates = @(Get-ResourceAggregates -Items @($records | ForEach-Object { $_ }))
    }
    $parent = Split-Path -Parent $OutputPath
    if ($parent -and -not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    $report | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath $OutputPath -Encoding utf8
}

Write-Output ("Wrote " + (Get-RedactedValue $OutputPath))
if ($failureCategories.Count -gt 0) {
    Write-Output ("Failure categories: " + (($failureCategories | Select-Object -Unique) -join ", "))
    exit 1
}
