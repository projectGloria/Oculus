param([switch]$IncludeHevc)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargoPath = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe" }
$outputDir = Join-Path ([System.IO.Path]::GetTempPath()) ("oculus-verify-" + [guid]::NewGuid().ToString("N"))

function Invoke-Checked {
    param([string]$File, [string[]]$Arguments)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$File failed with exit code $LASTEXITCODE" }
}

function Assert-Media {
    param([string]$Path, [string]$Codec, [double]$MinimumSeconds, [switch]$Partial)
    $probe = & ffprobe -v error -show_entries stream=codec_type,codec_name,width,height,duration -of json $Path
    if ($LASTEXITCODE -ne 0) { throw "ffprobe rejected $Path" }
    $streams = ($probe | ConvertFrom-Json).streams
    $video = @($streams | Where-Object { $_.codec_type -eq "video" })
    if ($video.Count -ne 1 -or $video[0].codec_name -ne $Codec) { throw "Unexpected video codec in $Path" }
    if ($video[0].width % 2 -or $video[0].height % 2) { throw "Odd video dimensions in $Path" }
    if ([double]$video[0].duration -lt $MinimumSeconds) { throw "Video too short in $Path" }
    $audio = @($streams | Where-Object { $_.codec_name -eq "aac" })
    if ($audio.Count -ne 1) { throw "Missing AAC stream in $Path" }
    if (-not $Partial -and [double]$audio[0].duration + 0.04 -lt [double]$video[0].duration) {
        throw "Audio ends more than 40 ms before video in $Path"
    }
    if (-not $Partial -and [double]$audio[0].duration - [double]$video[0].duration -gt 0.08) {
        throw "Audio extends more than 80 ms after video in $Path"
    }
    # Preserve the demuxer's time base: rounding VFR timestamps to an inferred frame
    # rate can create false duplicate-DTS errors in ffmpeg's null output muxer.
    Invoke-Checked ffmpeg @("-nostdin", "-v", "error", "-xerror", "-i", $Path,
        "-enc_time_base", "demux", "-fps_mode", "passthrough", "-f", "null", "-")
    Invoke-Checked ffmpeg @("-nostdin", "-v", "error", "-xerror", "-ss", "1", "-i", $Path,
        "-an", "-frames:v", "1", "-f", "null", "-")
}

function Assert-WriterOverload {
    param([string]$ExePath, [string]$Directory)
    $path = Join-Path $Directory "writer-overload.mp4"
    $stdout = Join-Path $Directory "writer-overload.stdout.log"
    $stderr = Join-Path $Directory "writer-overload.stderr.log"
    $arguments = @("--selftest", ('"' + $path + '"'), "--secs", "10", "--sys", "--mic", "--stall-writer-ms", "2000")
    $process = Start-Process -FilePath $ExePath -ArgumentList $arguments -Wait -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if ($process.ExitCode -ne 1) { throw "Writer overload was not reported as a recording failure" }
    if ((Get-Content -LiteralPath $stderr -Raw) -notlike "*recording buffer filled*") { throw "Missing overload explanation" }
    if ((Get-Content -LiteralPath $stdout -Raw) -notlike "*writer peak 128*") { throw "Writer queue limit was not reached" }
    Assert-Media $path "h264" 0.01 -Partial
}

Push-Location $repoRoot
try {
    if (@(Get-Process oculus -ErrorAction SilentlyContinue).Count) {
        throw "Close Oculus before verification: startup/quit checks need their own primary app instance."
    }
    Get-Command ffprobe, ffmpeg -ErrorAction Stop | Out-Null
    New-Item -ItemType Directory -Path $outputDir | Out-Null
    Invoke-Checked $cargoPath @("test", "--manifest-path", "src-tauri/Cargo.toml")
    Invoke-Checked $cargoPath @("clippy", "--manifest-path", "src-tauri/Cargo.toml", "--all-targets", "--", "-D", "warnings")
    Invoke-Checked npm.cmd @("run", "build")
    Invoke-Checked npm.cmd @("test")
    Invoke-Checked $cargoPath @("build", "--manifest-path", "src-tauri/Cargo.toml")
    $exePath = Join-Path $repoRoot "src-tauri\target\debug\oculus.exe"

    $normalPath = Join-Path $outputDir "normal.mp4"
    Invoke-Checked $exePath @("--selftest", $normalPath, "--secs", "8", "--pause", "2,3", "--sys", "--mic")
    Assert-Media $normalPath "h264" 4
    $pausedStopPath = Join-Path $outputDir "stop-while-paused.mp4"
    Invoke-Checked $exePath @("--selftest", $pausedStopPath, "--secs", "3", "--pause", "1,30", "--sys", "--mic")
    Assert-Media $pausedStopPath "h264" 0.5
    Assert-WriterOverload $exePath $outputDir

    $crashPath = Join-Path $outputDir "interrupted.mp4"
    & $exePath --selftest $crashPath --secs 15 --kill-at 8 --sys --mic
    if ($LASTEXITCODE -ne 86) { throw "Hard-exit test did not terminate at the requested point" }
    Assert-Media $crashPath "h264" 5 -Partial

    if ($IncludeHevc) {
        $hevcPath = Join-Path $outputDir "hevc.mp4"
        Invoke-Checked $exePath @("--selftest", $hevcPath, "--secs", "3", "--hevc", "--sys", "--mic")
        Assert-Media $hevcPath "hevc" 2
    }

    $cancelDir = Join-Path $outputDir "cancel-start"
    $cancelOutput = @(Invoke-Checked $exePath @("--selftest-app", $cancelDir, "--cancel-start"))
    $cancelOutput | Write-Output
    if ($cancelOutput -notcontains "startup cancellation completed") { throw "Startup-cancellation driver did not run" }
    if (@(Get-ChildItem -LiteralPath $cancelDir -Filter *.mp4).Count -ne 0) { throw "Cancelled startup retained a recording" }

    $quitDir = Join-Path $outputDir "slow-quit"
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    Invoke-Checked $exePath @("--selftest-app", $quitDir, "--finalize-delay-ms", "7000")
    $timer.Stop()
    if ($timer.Elapsed.TotalSeconds -lt 9) { throw "Quit exited before delayed finalization completed" }
    $saved = @(Get-ChildItem -LiteralPath $quitDir -Filter *.mp4)
    if ($saved.Count -ne 1) { throw "Quit test did not save one recording" }
    Assert-Media $saved[0].FullName "h264" 1.5

    Write-Output "Verification passed. Local test artifacts: $outputDir"
} finally {
    Pop-Location
}
