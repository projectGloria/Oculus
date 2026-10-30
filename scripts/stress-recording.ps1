param(
    [ValidateRange(1, 100)][int]$Repetitions = 6,
    [ValidateRange(30, 7200)][int]$Seconds = 300
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$exePath = Join-Path $repoRoot "src-tauri\target\debug\oculus.exe"
$outputDir = Join-Path ([IO.Path]::GetTempPath()) ("oculus-stress-" + [guid]::NewGuid().ToString("N"))
$results = [Collections.Generic.List[object]]::new()
$outputDrive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($outputDir))

function Assert-Media {
    param([string]$Path)
    $probe = & ffprobe -v error -count_packets -show_entries stream=codec_type,codec_name,width,height,duration,nb_read_packets -of json $Path
    if ($LASTEXITCODE -ne 0) { throw "ffprobe rejected the recording" }
    $streams = ($probe | ConvertFrom-Json).streams
    $video = @($streams | Where-Object { $_.codec_type -eq "video" })
    $audio = @($streams | Where-Object { $_.codec_type -eq "audio" })
    if ($video.Count -ne 1 -or $video[0].codec_name -ne "h264") { throw "Expected one H.264 stream" }
    if ($audio.Count -ne 1 -or $audio[0].codec_name -ne "aac") { throw "Expected one AAC stream" }
    $delta = [double]$audio[0].duration - [double]$video[0].duration
    if ($delta -lt -0.04 -or $delta -gt 0.08) { throw "Audio/video endpoints differ by $delta seconds" }
    if ([math]::Abs([double]$video[0].nb_read_packets - 60 * [double]$video[0].duration) -gt 2) {
        throw "Expected a complete 60 fps frame grid"
    }
    & ffmpeg -nostdin -v error -xerror -i $Path -enc_time_base demux -fps_mode passthrough -f null -
    if ($LASTEXITCODE -ne 0) { throw "Full media decode failed" }
    $seek = ([double]$video[0].duration * 0.8).ToString("0.000", [Globalization.CultureInfo]::InvariantCulture)
    & ffmpeg -nostdin -v error -xerror -ss $seek -i $Path -an -frames:v 1 -f null -
    if ($LASTEXITCODE -ne 0) { throw "Seek near the end failed" }
    return [pscustomobject]@{ VideoSeconds = [double]$video[0].duration; AudioSeconds = [double]$audio[0].duration; VideoFrames = [long]$video[0].nb_read_packets }
}

function Measure-Recording {
    param([string]$Name, [int]$Duration, [string]$Pause)
    $media = Join-Path $outputDir ($Name + ".mp4")
    $stdout = Join-Path $outputDir ($Name + ".stdout.log")
    $stderr = Join-Path $outputDir ($Name + ".stderr.log")
    $resourcePath = Join-Path $outputDir ($Name + ".resources.csv")
    $samples = [Collections.Generic.List[object]]::new()
    $arguments = @("--selftest", ('"' + $media + '"'), "--secs", "$Duration", "--pause", $Pause, "--sys", "--mic")
    Write-Output "Starting $Name ($Duration seconds)"
    $process = Start-Process -FilePath $exePath -ArgumentList $arguments -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # Keep a process handle open before exit; Windows PowerShell can otherwise lose ExitCode
    # when WaitForExit(Int32) observes completion before Process has opened its handle.
    $null = $process.Handle
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $nextSample = 0
    $nextProgress = 30
    $failure = $null
    $mediaInfo = $null
    try {
        if ($outputDrive.AvailableFreeSpace -lt 2GB) { throw "Less than 2 GiB free for stress artifacts" }
        while (-not $process.WaitForExit(1000)) {
            if ($timer.Elapsed.TotalSeconds -gt $Duration + 90) { throw "Recording/finalization timed out" }
            if ($timer.Elapsed.TotalSeconds -ge $nextSample) {
                $process.Refresh()
                $sample = [pscustomobject]@{
                    Seconds = [math]::Round($timer.Elapsed.TotalSeconds, 2)
                    PrivateMiB = [math]::Round($process.PrivateMemorySize64 / 1MB, 2)
                    WorkingSetMiB = [math]::Round($process.WorkingSet64 / 1MB, 2)
                    Handles = $process.HandleCount
                    OutputMiB = if (Test-Path -LiteralPath $media) { [math]::Round((Get-Item -LiteralPath $media).Length / 1MB, 2) } else { 0 }
                    FreeGiB = [math]::Round($outputDrive.AvailableFreeSpace / 1GB, 2)
                }
                $samples.Add($sample)
                $sample | Export-Csv -LiteralPath $resourcePath -Append -NoTypeInformation
                $nextSample += 5
                if ($outputDrive.AvailableFreeSpace -lt 1GB) {
                    throw "Less than 1 GiB free; ending stress run and retaining partial output"
                }
            }
            if ($timer.Elapsed.TotalSeconds -ge $nextProgress) {
                Write-Output "$Name elapsed $([int]$timer.Elapsed.TotalSeconds)s"
                $nextProgress += 30
            }
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw "Recorder exited with code $($process.ExitCode); inspect $stderr" }
        $mediaInfo = Assert-Media $media
        $pauseParts = $Pause.Split(',')
        $minimumDuration = $Duration - [double]$pauseParts[1] - 1
        if ($mediaInfo.VideoSeconds -lt $minimumDuration) { throw "Recording ended early" }
    } catch {
        $failure = $_.Exception.Message
    } finally {
        if (-not $process.HasExited) {
            # Only terminate the process created for this test, retaining its partial file/logs.
            $process.Kill()
            $process.WaitForExit()
        }
        if ($samples.Count) { $samples | Export-Csv -LiteralPath $resourcePath -NoTypeInformation }
        $process.Dispose()
    }
    $steady = @($samples | Where-Object { $_.Seconds -ge 20 })
    $startSamples = @($steady | Select-Object -First 3)
    $endSamples = @($steady | Select-Object -Last 3)
    $memoryGrowth = $null
    $handleGrowth = $null
    if ($steady.Count -ge 6) {
        $memoryGrowth = [math]::Round(($endSamples | Measure-Object PrivateMiB -Average).Average -
            ($startSamples | Measure-Object PrivateMiB -Average).Average, 2)
        $handleGrowth = [math]::Round(($endSamples | Measure-Object Handles -Average).Average -
            ($startSamples | Measure-Object Handles -Average).Average, 2)
        if (-not $failure -and ($memoryGrowth -gt 64 -or $handleGrowth -gt 16)) {
            $failure = "Resource growth exceeds investigation thresholds (64 MiB / 16 handles after warm-up)"
        }
    }
    $result = [pscustomobject]@{
        Name = $Name
        Passed = ($null -eq $failure)
        Error = $failure
        VideoSeconds = if ($mediaInfo) { $mediaInfo.VideoSeconds } else { $null }
        AudioSeconds = if ($mediaInfo) { $mediaInfo.AudioSeconds } else { $null }
        VideoFrames = if ($mediaInfo) { $mediaInfo.VideoFrames } else { $null }
        PrivateGrowthMiB = $memoryGrowth
        HandleGrowth = $handleGrowth
        Samples = $samples.Count
        WriterSummary = if (Test-Path -LiteralPath $stdout) {
            @(Get-Content -LiteralPath $stdout | Where-Object { $_ -match '^(queues:|sample writes:)' }) -join '; '
        } else { "" }
    }
    $results.Add($result)
    $results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $outputDir "results.json") -Encoding UTF8
    Write-Output ($result | ConvertTo-Json -Compress)
}

Push-Location $repoRoot
try {
    if (-not (Test-Path -LiteralPath $exePath)) { throw "Build the debug recorder before running stress checks" }
    Get-Command ffprobe, ffmpeg -ErrorAction Stop | Out-Null
    New-Item -ItemType Directory -Path $outputDir | Out-Null
    Write-Output "Local artifacts: $outputDir"
    $sha256 = [Security.Cryptography.SHA256]::Create()
    $exeStream = [IO.File]::OpenRead($exePath)
    try {
        $exeHash = [BitConverter]::ToString($sha256.ComputeHash($exeStream)).Replace("-", "")
    } finally {
        $exeStream.Dispose()
        $sha256.Dispose()
    }
    $metadata = [ordered]@{
        StartedAtUtc = [DateTime]::UtcNow.ToString("o")
        Repetitions = $Repetitions
        LongWallSeconds = $Seconds
        OperatingSystem = [Environment]::OSVersion.VersionString
        ExeSha256 = $exeHash
        FreeGiB = [math]::Round($outputDrive.AvailableFreeSpace / 1GB, 2)
        GpuDrivers = @(Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion, VideoModeDescription)
    }
    $metadata | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $outputDir "environment.json") -Encoding UTF8
    for ($iteration = 1; $iteration -le $Repetitions; $iteration++) {
        Measure-Recording "repeat-$iteration" 8 "2,3"
    }
    $pauseAt = [math]::Min(60, [int][math]::Floor($Seconds / 3))
    Measure-Recording "long" $Seconds "$pauseAt,5"
    if (@($results | Where-Object { -not $_.Passed }).Count) { throw "Stress checks found failures; inspect $outputDir\results.json" }
    Write-Output "Stress checks passed ($Seconds-second long run). Duration matching is not an acoustic synchronization test."
} finally {
    Pop-Location
}
