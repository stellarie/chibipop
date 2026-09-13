function New-OcrThresholds {
    $metrics = @(
        "working_set_bytes", "private_bytes", "latency_p95_ms",
        "cpu_peak_percent", "threads_peak", "handles_peak"
    )
    $result = [ordered]@{}
    foreach ($metric in $metrics) {
        $result[$metric] = [ordered]@{
            warning = [ordered]@{ enabled = $false; value = $null; operator = "delta_percent_gte" }
            failure = [ordered]@{ enabled = $false; value = $null; operator = "delta_percent_gte" }
        }
    }
    return $result
}

function Get-OcrThresholdState {
    param(
        [AllowNull()][Nullable[double]]$DeltaPercent,
        [object]$Threshold
    )
    if ($null -eq $Threshold -or -not $Threshold.enabled -or $null -eq $Threshold.value) {
        return "disabled"
    }
    if ($null -eq $DeltaPercent) { return "not-evaluable" }
    if ($Threshold.operator -eq "delta_percent_gte" -and $DeltaPercent -ge $Threshold.value) {
        return "exceeded"
    }
    return "within"
}
