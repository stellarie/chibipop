# M0 finding: `Windows.Media.Ocr` Japanese availability

Date: 2026-07-26
Host: this dev machine (Windows 10 IoT Enterprise LTSC 2021, build 10.0.19044)

## Step 1: Probe

Command run:

```
powershell -NoProfile -Command "[Windows.Media.Ocr.OcrEngine,Windows.Media,ContentType=WindowsRuntime] | Out-Null; [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | ForEach-Object { $_.LanguageTag }"
```

Complete output, verbatim:

```
en-US
ja
```

The WinRT type accelerator resolved without error on the first attempt — no `Add-Type -AssemblyName System.Runtime.WindowsRuntime` fallback was needed.

## Step 2: Record the finding

`ja` is present in `AvailableRecognizerLanguages`. Per the task brief, the `Get-WindowsCapability` check is only required when `ja` is absent, so it was not run — there is nothing to record there.

## Verdict

VERDICT: ja available — OCR tier viable as designed
