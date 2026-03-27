Add-Type -AssemblyName System.Speech
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$outPath = Join-Path $PSScriptRoot "test_speech.wav"

# Force 16kHz 16-bit mono WAV (matches Volcengine default)
$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)

# Try to find a Chinese voice
$zhVoice = $synth.GetInstalledVoices() | Where-Object { $_.VoiceInfo.Culture.Name -like 'zh-*' } | Select-Object -First 1
if ($zhVoice) {
    $synth.SelectVoice($zhVoice.VoiceInfo.Name)
    Write-Host "Using voice: $($zhVoice.VoiceInfo.Name)"
} else {
    Write-Host "No Chinese voice found, using default"
}

$synth.SetOutputToWaveFile($outPath, $format)
$synth.Speak("today is a good day")
$synth.Dispose()

$bytes = [System.IO.File]::ReadAllBytes($outPath)
$sr = [BitConverter]::ToInt32($bytes, 24)
$fileSize = (Get-Item $outPath).Length
Write-Host "Generated: $outPath ($fileSize bytes, ${sr}Hz)"
