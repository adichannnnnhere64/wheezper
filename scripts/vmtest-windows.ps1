<#
.SYNOPSIS
  End-to-end test of a *shipped* wheezper.exe on a clean, unprivileged
  Windows account.

.DESCRIPTION
  Verifies, from a user-writable directory with no installer, no PATH changes
  and no administrator rights, that the release artifact:

    * is running unelevated (the test asserts it is a standard user),
    * runs --version / --help,
    * enumerates audio devices on a machine that has none,
    * downloads its whisper model into %APPDATA% and transcribes real speech
      (a committed WAV fixture is converted to the f32le stream wheezper reads
      on stdin, so no audio hardware and no ffmpeg are needed),
    * writes the transcript to the notepad it was told to use,
    * reuses the cached model on a second run.

  If the machine has a SAPI voice, it additionally re-renders the sentence with
  Windows TTS and transcribes that.

  Run it in Windows Sandbox, a Hyper-V/VMware VM, or as a standard user on a CI
  runner (see .github/workflows/ci.yml).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File vmtest-windows.ps1 -Wav bench\fixtures\jfk.wav
#>
[CmdletBinding()]
param(
    [string]$Exe = (Join-Path $PSScriptRoot 'wheezper.exe'),
    [string]$Wav = (Join-Path $PSScriptRoot 'jfk.wav'),
    [string]$WorkDir = (Join-Path $env:USERPROFILE 'wheezper-vmtest'),
    [string]$Model = 'base',
    [switch]$KeepTemp
)

$ErrorActionPreference = 'Stop'
$script:failures = 0

function Step([string]$name) { Write-Host "`n=== $name" }
function Ok([string]$msg)   { Write-Host "  [ok]   $msg" }
function Fail([string]$msg) { Write-Host "  [FAIL] $msg"; $script:failures++ }
function Check([bool]$cond, [string]$msg) {
    if ($cond) { Ok $msg } else { Fail $msg }
}

function Assert-Unprivileged {
    Step 'running without system privileges'
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    $elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $adminSid = 'S-1-5-32-544'   # BUILTIN\Administrators
    $isAdminGroup = @($id.Groups | ForEach-Object { $_.Value }) -contains $adminSid
    Check (-not $elevated) 'process token is not elevated'
    Check (-not $isAdminGroup) 'user is not a member of BUILTIN\Administrators'
    if ($elevated -or $isAdminGroup) {
        throw "this test must run as a standard user (got $($id.Name)); refusing to continue"
    }
    Ok "user: $($id.Name)  profile: $env:USERPROFILE"
}

function Assert-NoInstallFootprint {
    Step 'no install / no system privileges required'
    Check (Test-Path $Exe) "exe exists: $Exe"
    Check ($Exe -notlike 'C:\Program Files*') 'exe runs from a user directory (not Program Files)'
    Check ($Exe -notlike 'C:\Windows*') 'exe is not under C:\Windows'
    $size = (Get-Item $Exe).Length
    # whisper.cpp + the embedded llama.cpp worker: a build without the worker
    # is ~6 MB, the shipped single-file build is ~10 MB
    Check ($size -gt 8MB) "exe has the LLM worker embedded ($([math]::Round($size/1MB,1)) MB)"
    $writable = $false
    try {
        $probe = Join-Path $WorkDir '.write-probe'
        Set-Content -Path $probe -Value 'ok' -Encoding ascii
        $writable = (Get-Content $probe) -eq 'ok'
        Remove-Item $probe -Force
    } catch { $writable = $false }
    Check $writable "work dir is writable by this user: $WorkDir"
}

function Run-Wheezper([string[]]$Arguments) {
    $out = Join-Path $WorkDir 'run.out'
    $err = Join-Path $WorkDir 'run.err'
    $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -WorkingDirectory $WorkDir `
        -RedirectStandardOutput $out -RedirectStandardError $err -NoNewWindow -Wait -PassThru
    [pscustomobject]@{
        ExitCode = $p.ExitCode
        Stdout   = (Get-Content $out -Raw -ErrorAction SilentlyContinue)
        Stderr   = (Get-Content $err -Raw -ErrorAction SilentlyContinue)
    }
}

function Convert-WavToF32([string]$InWav, [string]$F32) {
    # minimal RIFF reader: wheezper --source stdin wants 16 kHz mono f32le
    $bytes = [System.IO.File]::ReadAllBytes($InWav)
    $pos = 12
    $fmt = $null
    $dataStart = -1; $dataLen = 0
    while ($pos -lt $bytes.Length - 8) {
        $id = [System.Text.Encoding]::ASCII.GetString($bytes, $pos, 4)
        $size = [System.BitConverter]::ToUInt32($bytes, $pos + 4)
        $pos += 8
        switch ($id) {
            'fmt ' {
                $fmt = [pscustomobject]@{
                    Tag      = [System.BitConverter]::ToUInt16($bytes, $pos)
                    Channels = [System.BitConverter]::ToUInt16($bytes, $pos + 2)
                    Rate     = [System.BitConverter]::ToInt32($bytes, $pos + 4)
                    Bits     = [System.BitConverter]::ToUInt16($bytes, $pos + 14)
                }
            }
            'data' { $dataStart = $pos; $dataLen = $size }
        }
        $pos += $size
        if ($pos % 2 -eq 1) { $pos++ }   # RIFF chunks are word aligned
    }
    if ($null -eq $fmt -or $dataStart -lt 0) { throw "not a usable WAV: $InWav" }
    if ($fmt.Tag -ne 1 -or $fmt.Bits -ne 16 -or $fmt.Channels -ne 1 -or $fmt.Rate -ne 16000) {
        throw ("unexpected WAV format: tag=$($fmt.Tag) bits=$($fmt.Bits) ch=$($fmt.Channels) rate=$($fmt.Rate)")
    }
    $n = [int]($dataLen / 2)
    $out = New-Object byte[] ($n * 4)
    for ($i = 0; $i -lt $n; $i++) {
        $s = [System.BitConverter]::ToInt16($bytes, $dataStart + 2 * $i)
        $f = [float]($s / 32768.0)
        [System.BitConverter]::GetBytes($f).CopyTo($out, 4 * $i)
    }
    [System.IO.File]::WriteAllBytes($F32, $out)
    return $n
}

function Speak-ToWav([string]$Text, [string]$OutWav) {
    Add-Type -AssemblyName System.Speech
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    try {
        $fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
            16000,
            [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
            [System.Speech.AudioFormat.AudioChannel]::Mono)
        $synth.SetOutputToWaveFile($OutWav, $fmt)
        $synth.Rate = 0
        $synth.Speak($Text)
    } finally {
        $synth.Dispose()
    }
}

function Normalize([string]$text) {
    $t = $text.ToLowerInvariant()
    $t = [regex]::Replace($t, "[^a-z0-9' ]", ' ')
    $t = $t.Replace("'", '')
    return ([regex]::Replace($t, '\s+', ' ')).Trim()
}

# ---------------------------------------------------------------------------
Assert-Unprivileged
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
Assert-NoInstallFootprint

Step 'basic CLI'
$version = Run-Wheezper @('--version')
Check ($version.ExitCode -eq 0) "--version exit code 0 (got $($version.ExitCode))"
Write-Host ("         " + (($version.Stdout + $version.Stderr) -split "`n" | Select-Object -First 1))
$help = Run-Wheezper @('--help')
Check ($help.ExitCode -eq 0) "--help exit code 0 (got $($help.ExitCode))"
Check (($help.Stdout + $help.Stderr) -match '--source') '--help lists --source'

Step 'device enumeration on a machine with no audio hardware'
$devices = Run-Wheezper @('--list-devices')
Check ($devices.ExitCode -eq 0) "--list-devices exit code 0 (got $($devices.ExitCode))"
($devices.Stdout + $devices.Stderr) -split "`n" | Select-Object -First 3 | ForEach-Object { Write-Host "         $_" }

Step 'transcribe real speech (model download as an unprivileged user)'
Check (Test-Path $Wav) "input WAV exists: $Wav"
$f32 = Join-Path $WorkDir 'speech.f32'
$samples = Convert-WavToF32 $Wav $f32
Ok ("converted to 16 kHz mono f32le: $samples samples ({0:N1} s)" -f ($samples / 16000.0))

$notes = Join-Path $WorkDir 'notes.md'
if (Test-Path $notes) { Remove-Item $notes -Force }
$r = Run-Wheezper @('--source', 'stdin', '--model', $Model, '--language', 'en',
                    '--min-silence', '0.4', '--notes', $notes)
Check ($r.ExitCode -eq 0) "transcription exit code 0 (got $($r.ExitCode))"
if ($r.ExitCode -ne 0) { Write-Host $r.Stderr }
Check (Test-Path $notes) "notepad written: $notes"

$modelDir = Join-Path $env:APPDATA 'wheezper'
$modelFile = Get-ChildItem $modelDir -Filter "*$Model*" -ErrorAction SilentlyContinue | Select-Object -First 1
Check ($null -ne $modelFile) "model downloaded under %APPDATA%: $modelDir"
Check (-not (Test-Path 'C:\ProgramData\wheezper')) 'nothing written to ProgramData (no machine-wide install)'
Check (-not (Test-Path 'C:\Program Files\wheezper')) 'nothing written to Program Files'

$transcript = ''
if (Test-Path $notes) { $transcript = Get-Content $notes -Raw }
($transcript -split "`n" | Where-Object { $_ -match '^\s*-\s' } | Select-Object -First 3) |
    ForEach-Object { Write-Host "         $_" }
$norm = Normalize $transcript
Check ($norm -match 'fellow americans') 'transcript contains "fellow Americans"'
Check ($norm -match 'ask not') 'transcript contains "ask not"'
Check ($norm -match 'country') 'transcript contains "country"'

Step 'second run reuses the cached model'
$notes2 = Join-Path $WorkDir 'notes2.md'
$r2 = Run-Wheezper @('--source', 'stdin', '--model', $Model, '--language', 'en',
                     '--min-silence', '0.4', '--notes', $notes2)
Check ($r2.ExitCode -eq 0) "second run exit code 0 (got $($r2.ExitCode))"
$norm2 = Normalize (Get-Content $notes2 -Raw -ErrorAction SilentlyContinue)
Check ($norm2 -match 'ask not') 'second run still transcribes'

Step 'optional: Windows TTS round-trip'
try {
    Add-Type -AssemblyName System.Speech
    $voices = (New-Object System.Speech.Synthesis.SpeechSynthesizer).GetInstalledVoices() |
        Where-Object { $_.Enabled }
    if (-not $voices) {
        Write-Host '  [skip] no SAPI voice installed on this image'
    } else {
        $ttsWav = Join-Path $WorkDir 'tts.wav'
        $ttsF32 = Join-Path $WorkDir 'tts.f32'
        $ttsNotes = Join-Path $WorkDir 'tts.md'
        Speak-ToWav 'The quick brown fox jumps over the lazy dog.' $ttsWav
        Convert-WavToF32 $ttsWav $ttsF32 | Out-Null
        $r3 = Run-Wheezper @('--source', 'stdin', '--model', $Model, '--language', 'en',
                             '--min-silence', '0.4', '--notes', $ttsNotes)
        $ttsNorm = Normalize (Get-Content $ttsNotes -Raw -ErrorAction SilentlyContinue)
        Check ($r3.ExitCode -eq 0) 'TTS transcription exit code 0'
        Check ($ttsNorm -match 'brown fox') 'TTS transcript contains "brown fox"'
        Check ($ttsNorm -match 'lazy dog') 'TTS transcript contains "lazy dog"'
    }
} catch {
    Fail "TTS round-trip failed: $_"
}

if (-not $KeepTemp) {
    Remove-Item -Recurse -Force $WorkDir -ErrorAction SilentlyContinue
}

Write-Host ''
if ($script:failures -gt 0) {
    Write-Host "FAILED: $($script:failures) check(s) failed" -ForegroundColor Red
    exit 1
}
Write-Host 'PASS: shipped wheezper.exe works on a clean, unprivileged Windows account' -ForegroundColor Green
exit 0
