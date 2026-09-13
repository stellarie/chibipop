[CmdletBinding()]
param(
    [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$OutputPath,
    [string]$CargoPath = "cargo",
    [string]$TestExecutablePath,
    [string]$BaselinePath,
    [string]$PluginDirectory,
    [int]$DurationSeconds = 60,
    [int]$SampleMilliseconds = 100,
    [int]$IdleMilliseconds = 300,
    [int]$PhaseHoldMilliseconds = 0,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "ocr_performance_thresholds.ps1")

if ($DurationSeconds -lt 1) {
    throw "DurationSeconds must be at least 1."
}
if ($SampleMilliseconds -lt 10) {
    throw "SampleMilliseconds must be at least 10."
}
if ($IdleMilliseconds -lt 0) {
    throw "IdleMilliseconds must not be negative."
}
$minimumPhaseHold = [long]$SampleMilliseconds * 2
if ($PhaseHoldMilliseconds -gt 0 -and $PhaseHoldMilliseconds -lt $minimumPhaseHold) {
    throw "PhaseHoldMilliseconds must cover two sample intervals."
}
$effectivePhaseHold = if ($PhaseHoldMilliseconds -gt 0) {
    $PhaseHoldMilliseconds
} else {
    [math]::Max($minimumPhaseHold, 1000)
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

function Get-RunnerIdentity {
    param([string]$Revision)

    $image = $env:ImageOS
    if (-not $image) { $image = "local" }
    $os = $env:RUNNER_OS
    if (-not $os) { $os = [Environment]::OSVersion.Platform.ToString() }
    $architecture = $env:RUNNER_ARCH
    if (-not $architecture) {
        if ([Environment]::Is64BitOperatingSystem) { $architecture = "X64" }
        else { $architecture = "X86" }
    }
    $safeRevision = $null
    if ($Revision -match '^[0-9a-fA-F]{7,64}$') { $safeRevision = $Revision }
    [ordered]@{
        image = $image
        os = $os
        architecture = $architecture
        build_revision = $safeRevision
    }
}

function Get-BuildRevision {
    try {
        $value = (& git -C $RepoRoot rev-parse HEAD 2>$null | Select-Object -First 1).Trim()
        if ($value -match '^[0-9a-fA-F]{7,64}$') {
            return $value
        }
    } catch {
        return ""
    }
    return ""
}

function Get-BackendDefinition {
    param([string]$Id)

    if ($Id -eq "windows-ocr") {
        return [ordered]@{ id = $Id; version = "system"; environment = "windows" }
    }
    return [ordered]@{ id = "meikiocr"; version = "manifest"; environment = "meikiocr" }
}

function Set-ProcessEnvironment {
    param(
        [hashtable]$Values,
        [string[]]$Names,
        [hashtable]$Previous
    )
    foreach ($name in $Names) {
        $Previous[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        $value = $null
        if ($Values.ContainsKey($name)) { $value = [string]$Values[$name] }
        [Environment]::SetEnvironmentVariable($name, $value, "Process")
    }
}

function Restore-ProcessEnvironment {
    param([hashtable]$Previous)
    foreach ($name in $Previous.Keys) {
        [Environment]::SetEnvironmentVariable($name, $Previous[$name], "Process")
    }
}

function Read-JsonFile {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        return $null
    }
    return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

function Get-OptionalProperty {
    param(
        [AllowNull()][object]$Object,
        [string]$Name
    )
    if ($null -eq $Object) {
        return $null
    }
    if ($Object -is [System.Collections.IDictionary] -and $Object.Contains($Name)) {
        return $Object[$Name]
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

function Get-BackendResult {
    param(
        [string]$BackendId,
        [string]$TestReportPath,
        [string]$ResourceReportPath,
        [int]$CommandExitCode
    )
    $testReport = Read-JsonFile $TestReportPath
    $resourceReport = Read-JsonFile $ResourceReportPath
    $defaultBackend = Get-BackendDefinition $BackendId
    $testResult = $null
    if ($null -ne $testReport) {
        foreach ($candidate in @(Get-OptionalProperty $testReport "backend_results")) {
            $candidateBackend = Get-OptionalProperty $candidate "backend"
            if ($null -ne $candidateBackend -and $candidateBackend.id -eq $BackendId) {
                $testResult = $candidate
                break
            }
        }
    }
    $warnings = New-Object 'System.Collections.Generic.List[string]'
    $failures = New-Object 'System.Collections.Generic.List[string]'
    $environmentUnavailable = $false
    if ($null -ne $testResult) {
        foreach ($category in @(Get-OptionalProperty $testResult "warning_categories")) {
            $warnings.Add([string]$category)
        }
        foreach ($category in @(Get-OptionalProperty $testResult "failure_categories")) {
            $failures.Add([string]$category)
        }
        $environmentUnavailable =
            (Get-OptionalProperty $testResult "status") -eq "unavailable" -or
            @((Get-OptionalProperty $testResult "failure_categories") |
                Where-Object { $_ -eq "backend-unavailable" }).Count -gt 0
        if ((Get-OptionalProperty $testResult "status") -eq "failed") {
            $failures.Add("benchmark-report-failed")
        }
    }
    if ($null -ne $resourceReport) {
        foreach ($category in @(Get-OptionalProperty $resourceReport "failure_categories")) {
            if (-not ($environmentUnavailable -and $category -eq "phase-resource-incomplete")) {
                $failures.Add([string]$category)
            }
        }
        if (@(Get-OptionalProperty $resourceReport "cleanup_remaining_process_ids").Count -gt 0) {
            $failures.Add("cleanup-survivor")
        }
        if ((Get-OptionalProperty $resourceReport "timed_out") -eq $true) {
            $failures.Add("timeout")
        }
        if (-not $environmentUnavailable) {
            $coverage = Get-OptionalProperty $resourceReport "phase_coverage"
            if ($null -eq $coverage -or
                (Get-OptionalProperty $coverage "applicable") -ne $true -or
                (Get-OptionalProperty $coverage "complete") -ne $true) {
                $failures.Add("phase-resource-incomplete")
            }
        }
    } else {
        $failures.Add("resource-report-missing")
    }
    $resourceFailures = @($resourceReport | ForEach-Object {
            Get-OptionalProperty $_ "failure_categories"
        } | ForEach-Object { [string]$_ })
    $onlyUnavailablePhaseFailure = $environmentUnavailable -and
        $resourceFailures.Count -gt 0 -and
        @($resourceFailures | Where-Object { $_ -ne "phase-resource-incomplete" }).Count -eq 0
    if ($CommandExitCode -ne 0 -and -not $onlyUnavailablePhaseFailure) {
        $failures.Add("benchmark-command")
    }
    if ($null -eq $testResult) {
        $testResult = [ordered]@{
            status = "failed"
            backend = $defaultBackend
            fixture = $null
            phases = @()
            recognitions = @()
            latency = $null
            stable_hashes = $null
            warning_categories = @()
            failure_categories = @("benchmark-report-missing")
        }
        $failures.Add("benchmark-report-missing")
    }
    $backend = Get-OptionalProperty $testResult "backend"
    if ($null -eq $backend) { $backend = $defaultBackend }
    if (-not $environmentUnavailable -and (Get-OptionalProperty $testResult "status") -eq "available") {
        $requiredPhaseNames = @(
            "cold-startup", "post-handshake-idle", "first-recognition",
            "repeated-identical-recognition", "uncached-recognition",
            "post-activity-steady-state"
        )
        $testPhaseNames = @((Get-OptionalProperty $testResult "phases") |
            ForEach-Object { [string](Get-OptionalProperty $_ "name") })
        if (@($requiredPhaseNames | Where-Object { $_ -notin $testPhaseNames }).Count -gt 0) {
            $failures.Add("benchmark-phase-incomplete")
        }
    }
    if ((Get-OptionalProperty $backend "id") -eq "meikiocr" -and
        (Get-OptionalProperty $backend "identity_complete") -ne $true) {
        $warnings.Add("identity-incomplete")
    }
    [ordered]@{
        backend = $backend
        status = $testResult.status
        test = $testResult
        resources = $resourceReport
        command_exit_code = $CommandExitCode
        warning_categories = @($warnings | Select-Object -Unique)
        failure_categories = @($failures | Select-Object -Unique)
    }
}

function Get-ComparableIdentity {
    param(
        [object]$BackendResult,
        [object]$Runner,
        [string]$FixtureHash
    )
    $backend = Get-OptionalProperty $BackendResult "backend"
    $threadSettingsValue = Get-OptionalProperty $backend "thread_settings"
    $threadSettings = if ($null -ne $threadSettingsValue) {
        $threadSettingsValue | ConvertTo-Json -Depth 8 -Compress
    } else { "{}" }
    $modelHashesValue = Get-OptionalProperty $backend "model_hashes"
    $modelHashes = if ($null -ne $modelHashesValue) {
        $modelHashesValue | ConvertTo-Json -Depth 8 -Compress
    } else { "{}" }
    $pluginHashesValue = Get-OptionalProperty $backend "plugin_hashes"
    $pluginHashes = if ($null -ne $pluginHashesValue) {
        $pluginHashesValue | ConvertTo-Json -Depth 8 -Compress
    } else { "{}" }
    $scalesValue = Get-OptionalProperty $backend "scales"
    $scales = if ($null -ne $scalesValue) {
        @($scalesValue) -join ","
    } else { "" }
    $runnerImage = Get-OptionalProperty $Runner "image"
    $runnerOs = Get-OptionalProperty $Runner "os"
    $runnerArchitecture = Get-OptionalProperty $Runner "architecture"
    $test = Get-OptionalProperty $BackendResult "test"
    $fixture = Get-OptionalProperty $test "fixture"
    $reportedFixtureHash = Get-OptionalProperty $fixture "sha256"
    $identityFixtureHash = if ($reportedFixtureHash) { [string]$reportedFixtureHash } else { $FixtureHash }
    $identityComplete = Test-BackendIdentityComplete $backend
    $configHash = Get-OptionalProperty $backend "config_sha256"
    $modelAssetCount = Get-OptionalProperty $backend "model_asset_count"
    [ordered]@{
        runner_image = [string]$runnerImage
        runner_os = [string]$runnerOs
        runner_architecture = [string]$runnerArchitecture
        backend_id = [string](Get-OptionalProperty $backend "id")
        backend_version = [string](Get-OptionalProperty $backend "version")
        language = [string](Get-OptionalProperty $backend "language")
        thread_settings = $threadSettings
        model_hashes = $modelHashes
        plugin_hashes = $pluginHashes
        config_sha256 = if ($configHash) { [string]$configHash } else { "null" }
        model_asset_count = if ($null -ne $modelAssetCount) { [string]$modelAssetCount } else { "0" }
        identity_complete = $identityComplete
        fixture_sha256 = $identityFixtureHash
        scales = $scales
    }
}

function Test-BackendIdentityComplete {
    param([object]$Backend)
    $backendId = Get-OptionalProperty $Backend "id"
    if ($backendId -ne "meikiocr") {
        return $true
    }
    if ((Get-OptionalProperty $Backend "identity_complete") -ne $true) {
        return $false
    }
    $configHash = Get-OptionalProperty $Backend "config_sha256"
    $modelHashes = Get-OptionalProperty $Backend "model_hashes"
    if ([string]::IsNullOrWhiteSpace([string]$configHash) -or $null -eq $modelHashes) {
        return $false
    }
    return @($modelHashes.PSObject.Properties).Count -gt 0
}

function Get-IdentityFingerprint {
    param([object]$Identity)
    return ($Identity | ConvertTo-Json -Depth 10 -Compress)
}

function Get-ResourcePeak {
    param(
        [object]$BackendResult,
        [string]$Property
    )
    $resources = Get-OptionalProperty $BackendResult "resources"
    if ($null -eq $resources) { return $null }
    $aggregates = Get-OptionalProperty $resources "aggregates"
    $values = @($aggregates |
        Where-Object {
            (Get-OptionalProperty $_ "role") -eq "total" -and
            $null -ne (Get-OptionalProperty $_ $Property)
        } |
        ForEach-Object {
            Get-OptionalProperty (Get-OptionalProperty $_ $Property) "peak"
        })
    if ($values.Count -eq 0) { return $null }
    return [double]($values | Measure-Object -Maximum).Maximum
}

function Get-MetricValue {
    param(
        [object]$BackendResult,
        [string]$Metric
    )
    switch ($Metric) {
        "working_set_bytes" { return Get-ResourcePeak $BackendResult "working_set_bytes" }
        "private_bytes" { return Get-ResourcePeak $BackendResult "private_bytes" }
        "latency_p95_ms" {
            $test = Get-OptionalProperty $BackendResult "test"
            $latency = Get-OptionalProperty $test "latency"
            $repeated = Get-OptionalProperty $latency "repeated_identical"
            if ($null -ne $repeated) {
                $p95 = Get-OptionalProperty $repeated "p95_ms"
                if ($null -ne $p95) { return [double]$p95 }
            }
            return $null
        }
        "cpu_peak_percent" { return Get-ResourcePeak $BackendResult "cpu_percent" }
        "threads_peak" { return Get-ResourcePeak $BackendResult "threads" }
        "handles_peak" { return Get-ResourcePeak $BackendResult "handles" }
    }
    return $null
}

function Compare-BackendResult {
    param(
        [object]$Current,
        [object]$Baseline,
        [object]$Runner,
        [object]$BaselineRunner,
        [string]$FixtureHash,
        [string]$BaselineFixtureHash,
        [object]$Thresholds
    )
    $currentIdentity = Get-ComparableIdentity $Current $Runner $FixtureHash
    $baselineIdentity = Get-ComparableIdentity $Baseline $BaselineRunner $BaselineFixtureHash
    $currentFingerprint = Get-IdentityFingerprint $currentIdentity
    $baselineFingerprint = Get-IdentityFingerprint $baselineIdentity
    $metricRows = New-Object 'System.Collections.Generic.List[object]'
    foreach ($metric in $Thresholds.Keys) {
        $currentValue = Get-MetricValue $Current $metric
        $baselineValue = Get-MetricValue $Baseline $metric
        $delta = $null
        $deltaPercent = $null
        if ($null -ne $currentValue -and $null -ne $baselineValue) {
            $delta = $currentValue - $baselineValue
            if ($baselineValue -ne 0) {
                $deltaPercent = 100.0 * $delta / $baselineValue
            }
        }
        $metricThreshold = $Thresholds[$metric]
        $metricRows.Add([ordered]@{
                metric = $metric
                current = $currentValue
                baseline = $baselineValue
                delta = $delta
                delta_percent = $deltaPercent
                warning = Get-OcrThresholdState $deltaPercent $metricThreshold.warning
                failure = Get-OcrThresholdState $deltaPercent $metricThreshold.failure
            })
    }
    $status = "not-comparable"
    $currentIdentityComplete = $currentIdentity.identity_complete
    $baselineIdentityComplete = $baselineIdentity.identity_complete
    $backendId = Get-OptionalProperty $Current.backend "id"
    $identityUsable = $backendId -ne "meikiocr" -or ($currentIdentityComplete -and $baselineIdentityComplete)
    if ($currentFingerprint -eq $baselineFingerprint -and $identityUsable) {
        $status = "comparable"
    }
    $currentRevision = Get-OptionalProperty $Runner "build_revision"
    $baselineRevision = Get-OptionalProperty $BaselineRunner "build_revision"
    [ordered]@{
        backend_id = $backendId
        status = $status
        current_identity = $currentIdentity
        baseline_identity = $baselineIdentity
        current_build_revision = $currentRevision
        baseline_build_revision = $baselineRevision
        build_revision_match_required = $false
        metrics = @($metricRows | ForEach-Object { $_ })
    }
}

$revision = Get-BuildRevision
$runner = Get-RunnerIdentity $revision
$fixturePath = Join-Path $RepoRoot "crates\chibipop-windows\tests\fixtures\japanese_bgra.bin"
$fixtureHash = if (Test-Path -LiteralPath $fixturePath) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $fixturePath).Hash.ToLowerInvariant()
} else {
    ""
}
$measureScript = Join-Path $PSScriptRoot "measure_ocr_resources.ps1"
$defaultPluginDirectory = Join-Path $RepoRoot "plugins\meikiocr"
if (-not $PluginDirectory -and (Test-Path -LiteralPath (Join-Path $defaultPluginDirectory "plugin.toml"))) {
    $PluginDirectory = $defaultPluginDirectory
}
$measureTarget = "cargo"
$testFilePath = $CargoPath
$testArguments = @(
    "test", "--release", "-p", "chibipop-windows", "--test", "ocr_performance",
    "--", "--ignored", "--nocapture", "--exact", "fixed_pixels_engine_latency"
)
if ($TestExecutablePath) {
    if (-not (Test-Path -LiteralPath $TestExecutablePath)) {
        throw "TestExecutablePath does not exist."
    }
    $measureTarget = "release-test-executable"
    $testFilePath = $TestExecutablePath
    $testArguments = @("--ignored", "--nocapture", "--exact", "fixed_pixels_engine_latency")
}
$testArgumentLine = $testArguments -join " "
$backendIds = @("windows-ocr", "meikiocr")
$thresholds = New-OcrThresholds

if ($DryRun) {
    [ordered]@{
        schema = "chibipop-ocr-performance/v1"
        mode = "report-only"
        measurement_target = $measureTarget
        sample_milliseconds = $SampleMilliseconds
        phase_hold_milliseconds = $effectivePhaseHold
        runner = $runner
        fixture = [ordered]@{
            id = "japanese_bgra.bin"
            sha256 = $fixtureHash
            width = 400
            height = 120
            pixel_format = "bgra8"
        }
        backends = @($backendIds | ForEach-Object {
                $backend = Get-BackendDefinition $_
                [ordered]@{
                    id = $backend.id
                    version = $backend.version
                    test_arguments = $testArguments
                    plugin_configured = if ($_ -eq "meikiocr") { [bool]$PluginDirectory } else { $false }
                }
            })
        baseline = [ordered]@{ provided = [bool]$BaselinePath; build_revision_match_required = $false }
        thresholds = $thresholds
        product_goals = [ordered]@{
            windows_working_set_mib = 100
            meikiocr_working_set_mib = 200
        }
    } | ConvertTo-Json -Depth 12
    exit 0
}

if (-not $OutputPath) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutputPath = Join-Path (Get-Location) "ocr-performance-$stamp.json"
}

$baselineReport = $null
$baselineError = $null
if ($BaselinePath) {
    try {
        $baselineReport = Read-JsonFile $BaselinePath
        if ($null -eq $baselineReport) { $baselineError = "baseline report is missing" }
        elseif (@(Get-OptionalProperty $baselineReport "backends").Count -eq 0) {
            $baselineError = "baseline report has no backends"
        }
    } catch {
        $baselineError = "baseline report is invalid"
    }
}

$shell = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
if (-not $shell) {
    $shell = (Get-Command powershell -ErrorAction Stop).Source
}
$workRoot = Join-Path ([IO.Path]::GetTempPath()) ("chibipop-ocr-performance-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $workRoot -Force | Out-Null
$results = New-Object 'System.Collections.Generic.List[object]'
$overallFailure = $false

try {
    foreach ($backendId in $backendIds) {
        $backend = Get-BackendDefinition $backendId
        $safeName = $backendId -replace '[^A-Za-z0-9]+', '-'
        $testReportPath = Join-Path $workRoot "$safeName-test.json"
        $resourceReportPath = Join-Path $workRoot "$safeName-resources.json"
        $phaseFile = Join-Path $workRoot "$safeName-phase.txt"
        $identityFile = Join-Path $workRoot "$safeName-identity.json"
        $environment = @{
            CHIBIPOP_OCR_PERF_BACKEND = if ($backendId -eq "windows-ocr") { "windows" } else { "meikiocr" }
            CHIBIPOP_OCR_PERF_REPORT = $testReportPath
            CHIBIPOP_OCR_PERF_PHASE_FILE = $phaseFile
            CHIBIPOP_OCR_PERF_BACKEND_FILE = $identityFile
            CHIBIPOP_OCR_PERF_IDLE_MS = [string]$IdleMilliseconds
            CHIBIPOP_OCR_PERF_PHASE_HOLD_MS = [string]$effectivePhaseHold
        }
        if ($backendId -eq "meikiocr" -and $PluginDirectory) {
            $environment.CHIBIPOP_BENCH_PLUGIN = $PluginDirectory
        }
        $previous = @{}
        Set-ProcessEnvironment -Values $environment -Names @(
            "CHIBIPOP_OCR_PERF_BACKEND", "CHIBIPOP_OCR_PERF_REPORT",
            "CHIBIPOP_OCR_PERF_PHASE_FILE", "CHIBIPOP_OCR_PERF_BACKEND_FILE",
            "CHIBIPOP_OCR_PERF_IDLE_MS", "CHIBIPOP_OCR_PERF_PHASE_HOLD_MS", "CHIBIPOP_BENCH_PLUGIN"
        ) -Previous $previous
        try {
            $measureArguments = @(
                "-NoProfile", "-File", $measureScript,
                "-FilePath", $testFilePath,
                "-ArgumentList", $testArgumentLine,
                "-DurationSeconds", [string]$DurationSeconds,
                "-SampleMilliseconds", [string]$SampleMilliseconds,
                "-Label", $backendId,
                "-OutputPath", $resourceReportPath,
                "-PhaseFile", $phaseFile,
                "-BackendIdentityFile", $identityFile,
                "-RequiredPhases", "cold-startup,post-handshake-idle,first-recognition,repeated-identical-recognition,uncached-recognition,post-activity-steady-state",
                "-BackendId", $backend.id,
                "-BackendVersion", $backend.version,
                "-RunnerImage", [string]$runner.image,
                "-RunnerOs", [string]$runner.os,
                "-RunnerArchitecture", [string]$runner.architecture,
                "-BuildRevision", [string]$revision,
                "-FixtureSha256", $fixtureHash
            )
            $output = (& $shell @measureArguments 2>&1 | Out-String)
            $exitCode = if ($null -eq $LASTEXITCODE) { 0 } else { [int]$LASTEXITCODE }
        } finally {
            Restore-ProcessEnvironment $previous
        }
        if ($output) {
            Write-Output ((Get-RedactedValue $output).Trim())
        }
        $result = Get-BackendResult -BackendId $backendId -TestReportPath $testReportPath `
            -ResourceReportPath $resourceReportPath -CommandExitCode $exitCode
        $results.Add($result)
        $hardFailures = @($result.failure_categories | Where-Object { $_ -ne "backend-unavailable" })
        if ($hardFailures.Count -gt 0) { $overallFailure = $true }
    }

    $comparisons = New-Object 'System.Collections.Generic.List[object]'
    if ($null -ne $baselineReport) {
        $baselineRunner = Get-OptionalProperty $baselineReport "runner"
        foreach ($result in @($results | ForEach-Object { $_ })) {
            $baselineResult = $null
            foreach ($candidate in @(Get-OptionalProperty $baselineReport "backends")) {
                $candidateBackend = Get-OptionalProperty $candidate "backend"
                if ($null -ne $candidateBackend -and $candidateBackend.id -eq $result.backend.id) {
                    $baselineResult = $candidate
                    break
                }
            }
            if ($null -eq $baselineResult) {
                $comparisons.Add([ordered]@{ backend_id = $result.backend.id; status = "baseline-backend-missing"; metrics = @() })
            } else {
                $baselineFixture = Get-OptionalProperty $baselineReport "fixture"
                $baselineFixtureHash = Get-OptionalProperty $baselineFixture "sha256"
                $comparisons.Add((Compare-BackendResult -Current $result -Baseline $baselineResult `
                        -Runner $runner -BaselineRunner $baselineRunner -FixtureHash $fixtureHash `
                        -BaselineFixtureHash $baselineFixtureHash -Thresholds $thresholds))
            }
        }
    }
    if ($null -ne $baselineError) { $overallFailure = $true }

    $categories = [ordered]@{
        warning = @(
            "working-set-observation", "private-bytes-observation", "latency-observation",
            "cpu-observation", "thread-growth-observation", "handle-growth-observation",
            "unstable-text-hash", "unstable-geometry-hash", "identity-incomplete"
        )
        failure = @(
            "backend-unavailable", "launch-error", "child-failure", "timeout", "benchmark-command",
            "benchmark-report-missing", "benchmark-report-failed", "resource-report-missing", "recognition-error",
            "cleanup-survivor", "cleanup-identity-unverified", "cleanup-identity-mismatch",
            "phase-resource-incomplete", "benchmark-phase-incomplete", "baseline-input"
        )
        policy = "Performance categories remain report-only until reviewed numeric baselines exist."
    }
    $failureCategories = New-Object 'System.Collections.Generic.List[string]'
    foreach ($result in @($results | ForEach-Object { $_ })) {
        foreach ($category in @($result.failure_categories | Where-Object { $_ -ne "backend-unavailable" })) {
            $failureCategories.Add([string]$category)
        }
    }
    if ($null -ne $baselineError) { $failureCategories.Add("baseline-input") }
    $report = [ordered]@{
        schema = "chibipop-ocr-performance/v1"
        generated_at = (Get-Date).ToUniversalTime().ToString("o")
        mode = "report-only"
        measurement_target = $measureTarget
        sample_milliseconds = $SampleMilliseconds
        phase_hold_milliseconds = $effectivePhaseHold
        runner = $runner
        fixture = [ordered]@{
            id = "japanese_bgra.bin"
            sha256 = $fixtureHash
            width = 400
            height = 120
            pixel_format = "bgra8"
        }
        comparability = [ordered]@{
            identity_fields = @(
                "runner.image", "runner.os", "runner.architecture", "backend.id", "backend.version",
                "backend.language", "backend.thread_settings", "backend.model_hashes",
                "backend.plugin_hashes", "backend.config_sha256", "backend.model_asset_count",
                "backend.identity_complete", "fixture.sha256", "backend.scales"
            )
            rule = "Compare reports only when every identity field matches."
            build_revision_recorded = $true
            build_revision_match_required = $false
        }
        baseline = [ordered]@{
            provided = [bool]$BaselinePath
            available = $null -ne $baselineReport
            build_revision_match_required = $false
            comparisons = @($comparisons | ForEach-Object { $_ })
        }
        backends = @($results | ForEach-Object { $_ })
        categories = $categories
        thresholds = [ordered]@{
            warning = @($thresholds.Keys | ForEach-Object { [ordered]@{ metric = $_; threshold = $thresholds[$_].warning } })
            failure = @($thresholds.Keys | ForEach-Object { [ordered]@{ metric = $_; threshold = $thresholds[$_].failure } })
            evaluation = "Generic delta evaluation is disabled until reviewed thresholds provide values."
        }
        failure_categories = @($failureCategories | Select-Object -Unique)
        product_goals = [ordered]@{
            windows_working_set_mib = [ordered]@{ target = 100; status = "not-enforced"; backend = "windows-ocr" }
            meikiocr_working_set_mib = [ordered]@{ target = 200; status = "not-enforced"; backend = "meikiocr" }
        }
        privacy = [ordered]@{
            included = @("committed fixture identity", "OCR text and canonical geometry hashes", "process-tree metrics")
            excluded = @("personal configs", "dictionary data", "screenshots", "Anki data", "absolute personal paths")
            path_policy = "Executable and command paths are reduced to non-personal names."
        }
    }
    $parent = Split-Path -Parent $OutputPath
    if ($parent -and -not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    $report | ConvertTo-Json -Depth 18 | Set-Content -LiteralPath $OutputPath -Encoding utf8
} finally {
    if (Test-Path -LiteralPath $workRoot) {
        Remove-Item -LiteralPath $workRoot -Recurse -Force
    }
}

Write-Output ("Wrote " + (Get-RedactedValue $OutputPath))
if ($overallFailure) {
    exit 1
}
