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
# The Vulkan build loads vulkan-1.dll when it starts. A graphics driver installs
# that loader; a build machine has no graphics driver. The test then borrows
# the loader from LunarG's runtime components. The loader alone is enough to
# start the app: Vulkan devices are first asked for when a model is loaded.
function Test-X64Image([string]$Path) {
  # Machine type in the PE header: 0x8664 is x64.
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $header = [System.BitConverter]::ToInt32($bytes, 60)
  return ([System.BitConverter]::ToUInt16($bytes, $header + 4) -eq 34404)
}

function Find-VulkanLoader {
  $system = Join-Path $env:SystemRoot 'System32\vulkan-1.dll'
  if (Test-Path -LiteralPath $system) { return $system }

  if ($env:VULKAN_SDK -and (Test-Path -LiteralPath $env:VULKAN_SDK)) {
    $inSdk = Get-ChildItem -LiteralPath $env:VULKAN_SDK -Recurse -Filter 'vulkan-1.dll' -ErrorAction SilentlyContinue |
      Where-Object { Test-X64Image $_.FullName } | Select-Object -First 1
    if ($inSdk) { return $inSdk.FullName }
  }

  $sdkVersion = if ($env:VULKAN_SDK_VERSION) { $env:VULKAN_SDK_VERSION } else { '1.4.309.0' }
  $zip = Join-Path $env:TEMP "vulkan-runtime-components-$sdkVersion.zip"
  $folder = Join-Path $env:TEMP "vulkan-runtime-components-$sdkVersion"
  $url = "https://sdk.lunarg.com/sdk/download/$sdkVersion/windows/vulkan-runtime-components.zip?Human=true"
  Write-Host "No Vulkan loader on this machine; fetching LunarG's runtime components: $url"
  & curl.exe --fail --location --silent --show-error --retry 2 --connect-timeout 30 --max-time 300 --output $zip $url
  if ($LASTEXITCODE -ne 0) {
    Write-Warning "The runtime components could not be downloaded (curl exit code $LASTEXITCODE)"
    return $null
  }
  Expand-Archive -LiteralPath $zip -DestinationPath $folder -Force
  $all = @(Get-ChildItem -LiteralPath $folder -Recurse -Filter 'vulkan-1.dll')
  $found = $all | Where-Object { Test-X64Image $_.FullName } | Select-Object -First 1
  if (-not $found) {
    Write-Warning "The runtime components contain no x64 vulkan-1.dll (found: $(($all | ForEach-Object { $_.FullName }) -join ', '))"
    return $null
  }
  $signature = Get-AuthenticodeSignature -LiteralPath $found.FullName
  Write-Host "Loader signature: $($signature.Status), $($signature.SignerCertificate.Subject)"
  if ($signature.Status -ne 'Valid') {
    Write-Warning 'The downloaded vulkan-1.dll has no valid signature and is not used'
    return $null
  }
  return $found.FullName
}

$loader = Find-VulkanLoader
if (-not $loader) {
  # Without a loader the program cannot start on this machine, whatever its
  # state. That says nothing about the installer, so it is a warning.
  Write-Host '::warning title=Start check skipped::This machine has no Vulkan loader (vulkan-1.dll) and none could be fetched. The installed app was not started.'
} else {
  $loaderFolder = Split-Path $loader -Parent
  if ($loaderFolder -ne (Join-Path $env:SystemRoot 'System32')) {
    Write-Host "Vulkan loader for this test: $loader"
    $env:PATH = "$loaderFolder;$env:PATH"
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

  # The file appears before the migrations have run. The app reports the end
  # of its first-run database setup in its log (database\commands.rs), which
  # goes to the redirected error stream.
  $setupReported = 'Fresh database initialized successfully'
  $setupDeadline = (Get-Date).AddSeconds(30)
  while (-not $exited -and (Get-Date) -lt $setupDeadline) {
    $reported = $false
    try { $reported = [bool](Select-String -LiteralPath $stderr -Pattern $setupReported -SimpleMatch -Quiet) } catch { $reported = $false }
    if ($reported) { break }
    Start-Sleep -Seconds 2
  }

  Get-Process -Name 'meetily-homebrew' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 2

  $logLines = @()
  foreach ($path in @($stdout, $stderr)) {
    if (Test-Path -LiteralPath $path) { $logLines += @(Get-Content -LiteralPath $path) }
  }
  Write-Host "--- app log: $($logLines.Count) lines; first 30 and last 30 ---"
  $logLines | Select-Object -First 30 | ForEach-Object { Write-Host $_ }
  if ($logLines.Count -gt 60) { Write-Host '  ...' }
  if ($logLines.Count -gt 30) { $logLines | Select-Object -Last ([Math]::Min(30, $logLines.Count - 30)) | ForEach-Object { Write-Host $_ } }
  $setupDone = [bool]($logLines | Where-Object { $_ -like "*$setupReported*" } | Select-Object -First 1)
  $panicked = $logLines | Where-Object { $_ -like '*panicked at*' } | Select-Object -First 1
  $dataFolder = Join-Path $installDir 'data'
  if (Test-Path -LiteralPath $dataFolder) {
    Write-Host "Data folder: $((Get-ChildItem -LiteralPath $dataFolder | ForEach-Object { $_.Name }) -join ', ')"
  }

  if ($exited) { throw "The app ended by itself after $seconds seconds (exit code $exitCode)" }
  if ($panicked) { throw "The app reported an internal error: $panicked" }
  if (-not $hasDatabase) { throw "The app ran for $seconds seconds without creating its database: the window did not load, or first-run setup did not start" }
  if (-not $setupDone) { throw 'The database file exists, but the app did not report a finished first-run database setup, which includes the migrations' }
  Write-Host "The app started, created its database after about $seconds seconds and finished first-run database setup: $database"
}

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
Write-Host $(if ($loader) { 'Smoke test passed.' } else { 'Install and folder checks passed; the start check was skipped.' })
