# Installs the freshly built installer on this machine, starts the app once and
# removes it again. Written for the build machine of
# .github/workflows/homebrew-windows.yml: a clean Windows without Meetily.
#
# It answers what a build log cannot:
#   - the installer runs unattended and puts the app into its own folder, not
#     into a stock Meetily's (the base's template names that folder literally);
#   - the installed program starts, and its window creates the database, which
#     needs the embedded frontend, WebView2 and every migration to work.
#
# It refuses to run where either app is already installed, so it never touches
# an existing installation or its data.
param(
  [string]$Installer,
  [int]$StartSeconds = 90
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
if (-not $Installer) {
  $found = Get-ChildItem -LiteralPath (Join-Path $repo 'dist-homebrew') -Filter '*-setup.exe' | Select-Object -First 1
  if (-not $found) { throw 'No installer found in dist-homebrew' }
  $Installer = $found.FullName
}

$installDir = Join-Path $env:LOCALAPPDATA 'Meetily-Homebrew'
$stockDir = Join-Path $env:LOCALAPPDATA 'Meetily-ActuallyFree'
$exe = Join-Path $installDir 'meetily-homebrew.exe'
$database = Join-Path $installDir 'data\meeting_minutes.sqlite'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Meetily Homebrew'

if (Test-Path -LiteralPath $installDir) { throw "Refusing to run: $installDir exists. This test is for a machine without the app." }
if (Test-Path -LiteralPath $stockDir) { throw "Refusing to run: a stock Meetily is installed at $stockDir." }

# --- Install -------------------------------------------------------------------
Write-Host "=== Silent install: $Installer ==="
$install = Start-Process -FilePath $Installer -ArgumentList '/S' -Wait -PassThru
if ($install.ExitCode -ne 0) { throw "The installer ended with exit code $($install.ExitCode)" }

$expected = @(
  'meetily-homebrew.exe',
  'uninstall.exe',
  'ffmpeg.exe',
  'llama-helper.exe',
  'binaries\onnxruntime\onnxruntime.dll',
  'binaries\onnxruntime\DirectML.dll',
  'resources\diarization\segmentation-3.0-fp16.onnx'
)
$missing = @($expected | Where-Object { -not (Test-Path -LiteralPath (Join-Path $installDir $_)) })
if ($missing.Count -gt 0) {
  Write-Host 'Installed files:'
  Get-ChildItem -LiteralPath $installDir -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $($_.FullName.Substring($installDir.Length + 1))" }
  throw "Installed files are missing: $($missing -join ', ')"
}
$templates = @(Get-ChildItem -LiteralPath (Join-Path $installDir 'templates') -Filter '*.json' -ErrorAction SilentlyContinue)
if ($templates.Count -eq 0) { throw 'No summary templates were installed' }
if (Test-Path -LiteralPath $stockDir) { throw "The installer created the stock app's folder: $stockDir" }
if (-not (Test-Path -LiteralPath $uninstallKey)) { throw "The installer did not register 'Meetily Homebrew' for removal" }
if (-not (Test-Path -LiteralPath (Join-Path $installDir 'DirectML.dll'))) {
  Write-Warning 'DirectML.dll is not beside the program; the app still loads its own copy under binaries\onnxruntime.'
}
$product = (Get-Item -LiteralPath $exe).VersionInfo.ProductName
Write-Host "Installed '$product' to $installDir; no stock folder was created."

# --- Start ---------------------------------------------------------------------
# The Vulkan build loads vulkan-1.dll, which a graphics driver installs. A build
# machine has no such driver; the Vulkan SDK's copy stands in for it here.
$systemLoader = Join-Path $env:SystemRoot 'System32\vulkan-1.dll'
if (-not (Test-Path -LiteralPath $systemLoader)) {
  $candidates = @()
  if ($env:VULKAN_SDK) {
    $candidates += Join-Path $env:VULKAN_SDK 'Bin\vulkan-1.dll'
    $candidates += Join-Path $env:VULKAN_SDK 'runtime\x64\vulkan-1.dll'
  }
  $sdkLoader = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
  if ($sdkLoader) {
    Write-Host "This machine has no Vulkan loader; using $sdkLoader for the test."
    $env:PATH = "$(Split-Path $sdkLoader -Parent);$env:PATH"
  } else {
    Write-Warning 'No vulkan-1.dll on this machine. The app can only start where a graphics driver provides it.'
  }
}

Write-Host "=== Start the app (up to $StartSeconds seconds) ==="
$stdout = Join-Path $env:TEMP 'meetily-homebrew-smoke-stdout.log'
$stderr = Join-Path $env:TEMP 'meetily-homebrew-smoke-stderr.log'
$started = Get-Date
$app = Start-Process -FilePath $exe -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
$null = $app.Handle  # keeps the exit code readable after the process has ended
$deadline = $started.AddSeconds($StartSeconds)
while ((Get-Date) -lt $deadline -and -not $app.HasExited -and -not (Test-Path -LiteralPath $database)) {
  Start-Sleep -Seconds 2
}
$seconds = [int]((Get-Date) - $started).TotalSeconds
$exited = $app.HasExited
$exitCode = if ($exited) { $app.ExitCode } else { $null }
$hasDatabase = Test-Path -LiteralPath $database
if (-not $exited) { Start-Sleep -Seconds 5 }  # lets first-run setup finish its writes

Get-Process -Name 'meetily-homebrew' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

function Show-Log([string]$Title, [string]$Path) {
  if ((Test-Path -LiteralPath $Path) -and (Get-Item -LiteralPath $Path).Length -gt 0) {
    Write-Host "--- $Title (first 80 lines) ---"
    Get-Content -LiteralPath $Path -TotalCount 80 | ForEach-Object { Write-Host $_ }
  }
}
Show-Log 'app output' $stdout
Show-Log 'app errors' $stderr

if ($exited) { throw "The app ended by itself after $seconds seconds (exit code $exitCode)" }
if (-not $hasDatabase) { throw "The app ran for $seconds seconds without creating its database: the window did not load, or first-run setup did not start" }
Write-Host "The app started and created its database after about $seconds seconds: $database"

# --- Uninstall (informational) ---------------------------------------------------
# "_?=" makes the uninstaller run in place, so this waits for it. It must be the
# last argument and unquoted.
Write-Host '=== Silent uninstall ==='
$uninstall = Start-Process -FilePath (Join-Path $installDir 'uninstall.exe') -ArgumentList '/S', "_?=$installDir" -Wait -PassThru
Write-Host "Uninstaller exit code: $($uninstall.ExitCode)"
if (Test-Path -LiteralPath $exe) { Write-Warning 'The program file is still present after the uninstall.' }
if (Test-Path -LiteralPath $installDir) {
  Write-Host 'Left in the install folder (the data folder is kept unless its removal is ticked):'
  Get-ChildItem -LiteralPath $installDir -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $($_.Name)" }
}
Write-Host 'Smoke test passed.'
