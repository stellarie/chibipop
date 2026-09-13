[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$measureScript = Join-Path $repoRoot "scripts\measure_ocr_resources.ps1"
$reportScript = Join-Path $repoRoot "scripts\ocr_performance_report.ps1"
. (Join-Path $repoRoot "scripts\ocr_performance_thresholds.ps1")
$shell = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
if (-not $shell) { $shell = (Get-Command powershell -ErrorAction Stop).Source }
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("chibipop-ocr-script-tests-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempRoot -Force | Out-Null
$passed = 0

function Assert-True {
    param(
        [bool]$Condition,
        [string]$Message
    )
    if (-not $Condition) { throw $Message }
}

function Invoke-TestScript {
    param(
        [string]$ScriptPath,
        [hashtable]$Parameters
    )
    $arguments = @("-NoProfile", "-File", $ScriptPath)
    foreach ($name in $Parameters.Keys) {
        $arguments += "-$name"
        $value = $Parameters[$name]
        if ($value -is [bool]) {
            if ($value) { continue }
        } elseif ($value -is [array]) { $arguments += @($value) }
        else { $arguments += [string]$value }
    }
    $output = (& $shell @arguments 2>&1 | Out-String)
    [pscustomobject]@{
        exit_code = if ($null -eq $LASTEXITCODE) { 0 } else { [int]$LASTEXITCODE }
        output = $output
    }
}

function Read-Report {
    param([string]$Path)
    Assert-True (Test-Path -LiteralPath $Path) "report was not written: $Path"
    return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

try {
    $enabledThreshold = @{ enabled = $true; value = 10; operator = "delta_percent_gte" }
    Assert-True ((Get-OcrThresholdState 5 $enabledThreshold) -eq "within") "within threshold state failed"
    Assert-True ((Get-OcrThresholdState 10 $enabledThreshold) -eq "exceeded") "exceeded threshold state failed"
    Assert-True ((Get-OcrThresholdState $null $enabledThreshold) -eq "not-evaluable") "not-evaluable state failed"
    $passed++

    $failurePath = Join-Path $tempRoot "child-failure.json"
    $failure = Invoke-TestScript $measureScript @{
        FilePath = "cmd.exe"
        ArgumentList = "/c exit 7"
        DurationSeconds = 3
        SampleMilliseconds = 50
        OutputPath = $failurePath
    }
    $failureReport = Read-Report $failurePath
    Assert-True ($failure.exit_code -ne 0) "child failure returned zero"
    Assert-True ($failureReport.child_exit_code -eq 7) "child exit code was not recorded"
    Assert-True (-not $failureReport.timed_out) "child failure was marked as timeout"
    Assert-True (@($failureReport.failure_categories) -contains "child-failure") "child failure category missing"
    Assert-True (@($failureReport.cleanup_remaining_process_ids).Count -eq 0) "child-failure cleanup left a survivor"
    $passed++

    $timeoutPath = Join-Path $tempRoot "timeout.json"
    $timeout = Invoke-TestScript $measureScript @{
        FilePath = "pwsh"
        ArgumentList = "-NoProfile -Command Start-Sleep -Seconds 3"
        DurationSeconds = 1
        SampleMilliseconds = 50
        OutputPath = $timeoutPath
    }
    $timeoutReport = Read-Report $timeoutPath
    Assert-True ($timeout.exit_code -ne 0) "timeout returned zero"
    Assert-True $timeoutReport.timed_out "timeout state was not recorded"
    Assert-True (@($timeoutReport.failure_categories) -contains "timeout") "timeout category missing"
    $passed++

    $phasePath = Join-Path $tempRoot "phase.txt"
    $phaseStart = [DateTimeOffset]::Now.ToUnixTimeMilliseconds()
    $phaseEnd = $phaseStart + 10000
    Set-Content -LiteralPath $phasePath -Value ("start`t$phaseStart`tshort-phase`nend`t$phaseEnd`tshort-phase") -NoNewline
    $phaseReportPath = Join-Path $tempRoot "phase.json"
    $phase = Invoke-TestScript $measureScript @{
        FilePath = "pwsh"
        ArgumentList = "-NoProfile -Command Start-Sleep -Seconds 1"
        DurationSeconds = 2
        SampleMilliseconds = 50
        PhaseFile = $phasePath
        RequiredPhases = "short-phase"
        OutputPath = $phaseReportPath
    }
    $phaseReport = Read-Report $phaseReportPath
    Assert-True ($phase.exit_code -eq 0) "phase sampler failed"
    Assert-True (@($phaseReport.phase_events).Count -eq 2) "phase events were not retained"
    Assert-True (@($phaseReport.records | Where-Object { $_.Phase -eq "short-phase" }).Count -gt 0) "phase was not assigned to samples"
    $totalAggregate = @($phaseReport.aggregates | Where-Object { $_.role -eq "total" }) | Select-Object -First 1
    Assert-True ($null -ne $totalAggregate) "total resource aggregate missing"
    foreach ($metric in @("working_set_bytes", "private_bytes", "cpu_seconds", "threads", "handles")) {
        $stats = $totalAggregate.$metric
        Assert-True ($null -ne $stats -and $null -ne $stats.median -and $null -ne $stats.p95 -and $null -ne $stats.peak) "$metric aggregate is incomplete"
    }
    $passed++

    $dryRun = Invoke-TestScript $measureScript @{
        FilePath = "C:\Users\secret\tool.exe"
        ArgumentList = "\\server\share\secret.exe"
        DryRun = $true
    }
    Assert-True ($dryRun.exit_code -eq 0) "redaction dry-run failed"
    Assert-True (-not $dryRun.output.Contains("C:\Users\secret")) "drive path leaked from dry-run"
    Assert-True (-not $dryRun.output.Contains("\\server\share")) "UNC path leaked from dry-run"
    $passed++

    $missingPath = Join-Path $tempRoot "missing-report.json"
    $missing = Invoke-TestScript $reportScript @{
        RepoRoot = $repoRoot
        CargoPath = "cmd.exe"
        OutputPath = $missingPath
        DurationSeconds = 1
        SampleMilliseconds = 50
        IdleMilliseconds = 10
    }
    $missingReport = Read-Report $missingPath
    Assert-True ($missing.exit_code -ne 0) "missing benchmark reports returned zero"
    Assert-True (@($missingReport.failure_categories) -contains "benchmark-report-missing") "missing report category absent"
    Assert-True (-not $missing.output.Contains($missingPath)) "report output leaked its absolute path"
    $passed++

    $releaseTest = Get-ChildItem (Join-Path $repoRoot "target\release\deps\ocr_performance-*.exe") |
        Sort-Object LastWriteTime | Select-Object -Last 1
    if ($null -ne $releaseTest) {
        $currentPath = Join-Path $tempRoot "baseline-current.json"
        $baselinePath = Join-Path $tempRoot "baseline-input.json"
        $comparedPath = Join-Path $tempRoot "baseline-compared.json"
        $current = Invoke-TestScript $reportScript @{
            RepoRoot = $repoRoot
            TestExecutablePath = $releaseTest.FullName
            OutputPath = $currentPath
            DurationSeconds = 20
            SampleMilliseconds = 50
            IdleMilliseconds = 20
        }
        Assert-True ($current.exit_code -eq 0) "baseline source report failed"
        $currentReport = Read-Report $currentPath
        foreach ($backend in @($currentReport.backends | Where-Object { $_.status -eq "available" })) {
            Assert-True ($backend.resources.backend.version -eq $backend.backend.version) "resource backend version drifted"
            Assert-True ($null -ne $backend.backend.language) "backend language identity missing"
            Assert-True ($null -ne $backend.backend.scales) "backend scale identity missing"
            Assert-True $backend.resources.phase_coverage.complete "resource phase coverage is incomplete"
            Assert-True (@($backend.resources.phase_coverage.missing).Count -eq 0) "resource phase rows are missing"
        }
        $currentReport.runner.build_revision = "0123456789abcdef0123456789abcdef01234567"
        $currentReport | ConvertTo-Json -Depth 18 | Set-Content -LiteralPath $baselinePath -Encoding utf8
        $compared = Invoke-TestScript $reportScript @{
            RepoRoot = $repoRoot
            TestExecutablePath = $releaseTest.FullName
            BaselinePath = $baselinePath
            OutputPath = $comparedPath
            DurationSeconds = 20
            SampleMilliseconds = 50
            IdleMilliseconds = 20
        }
        $comparedReport = Read-Report $comparedPath
        Assert-True ($compared.exit_code -eq 0) "cross-revision baseline returned failure"
        Assert-True (@($comparedReport.baseline.comparisons)[0].status -eq "comparable") "matching baseline was rejected"
        Assert-True (-not $comparedReport.baseline.build_revision_match_required) "build revision blocked comparison"
        $incomplete = @($comparedReport.baseline.comparisons | Where-Object { $_.backend_id -eq "meikiocr" })
        if ($incomplete.Count -gt 0) {
            Assert-True ($incomplete[0].status -eq "not-comparable") "incomplete MeikiOCR identity compared"
        }
        Assert-True (@($comparedReport.thresholds.warning | Where-Object { $_.threshold.enabled }).Count -eq 0) "warning threshold was enabled"
        Assert-True (@($comparedReport.thresholds.failure | Where-Object { $_.threshold.enabled }).Count -eq 0) "failure threshold was enabled"
        $passed++
    } else {
        Write-Output "SKIP: release benchmark executable is unavailable for baseline comparison"
    }

    Write-Output "PowerShell OCR performance tests passed: $passed"
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
