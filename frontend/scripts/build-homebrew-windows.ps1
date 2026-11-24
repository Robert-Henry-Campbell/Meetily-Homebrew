# Builds the unsigned Windows installer of this personal build: one variant,
# Vulkan (AMD, Intel and compatible NVIDIA graphics).
#
# Used by .github/workflows/homebrew-windows.yml and usable on a Windows PC
# with Visual Studio Build Tools 2022, Rust, Node and pnpm installed. It
# follows build-labs-test-windows.ps1 (environment) and the Vulkan part of
# build-universal-windows.ps1 (features), without the CUDA and CPU variants,
# the bootstrapper, code signing and updater artifacts.
param(
  [switch]$SkipFrontend
)

$ErrorActionPreference = 'Stop'
$frontend = Split-Path $PSScriptRoot -Parent
$repo = Split-Path $frontend -Parent
$tauri = Join-Path $frontend 'src-tauri'
$config = Get-Content (Join-Path $tauri 'tauri.conf.json') -Raw | ConvertFrom-Json
$version = $config.version
$product = $config.productName
$mainBinary = "$($config.mainBinaryName).exe"
if (-not $config.mainBinaryName) { throw 'tauri.conf.json has no mainBinaryName; this script is for the homebrew branch' }

function Invoke-Checked([string]$What, [scriptblock]$Command) {
  Write-Host ""
  Write-Host "=== $What ==="
  & $Command
  if ($LASTEXITCODE -ne 0) { throw "$What failed (exit code $LASTEXITCODE)" }
}

# --- C++ toolchain -----------------------------------------------------------
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio locator (vswhere.exe) is missing' }
$vsInstall = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
$vcvars = Join-Path $vsInstall 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path -LiteralPath $vcvars)) { throw "Visual Studio x64 tools are missing: $vcvars" }
# Import the C++ environment into this PowerShell process for whisper.cpp.
cmd /c "call `"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
  if ($_ -match '^([^=]+)=(.*)$') {
    [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
  }
}
$env:CMAKE_GENERATOR = 'Ninja'
Remove-Item Env:CMAKE_GENERATOR_INSTANCE -ErrorAction SilentlyContinue
$ninja = Join-Path $vsInstall 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja'
$env:PATH = "$ninja;$env:PATH"

# --- LLVM 18 (whisper-rs generates its Windows bindings with libclang) --------
$llvm = if ($env:MEETILY_LLVM_DIR) { $env:MEETILY_LLVM_DIR } else { 'C:\Program Files\LLVM' }
$clang = Join-Path $llvm 'bin\clang.exe'
$clangVersion = if (Test-Path -LiteralPath $clang) { (& $clang --version | Select-Object -First 1) } else { '' }
if ($clangVersion -notmatch 'version 18\.') {
  Write-Host "LLVM at '$llvm' is '$clangVersion'; fetching LLVM 18"
  $llvm = & (Join-Path $PSScriptRoot 'bootstrap-llvm18.ps1') | Select-Object -Last 1
}
$env:LIBCLANG_PATH = Join-Path $llvm 'bin'
if (-not (Test-Path -LiteralPath (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) { throw "libclang.dll not found under $llvm" }
Remove-Item Env:WHISPER_DONT_GENERATE_BINDINGS -ErrorAction SilentlyContinue
Write-Host "LIBCLANG_PATH = $env:LIBCLANG_PATH"

# --- Vulkan SDK ---------------------------------------------------------------
$vulkanSdk = $env:VULKAN_SDK
if (-not $vulkanSdk -or -not (Test-Path -LiteralPath (Join-Path $vulkanSdk 'Bin\glslc.exe'))) {
  Write-Host 'VULKAN_SDK has no glslc; building the portable Vulkan build tools'
  $vulkanSdk = & (Join-Path $PSScriptRoot 'bootstrap-vulkan-build-tools.ps1') | Select-Object -Last 1
}
$vulkanSdk = [System.IO.Path]::GetFullPath($vulkanSdk)
$env:VULKAN_SDK = $vulkanSdk
$env:VK_SDK_PATH = $vulkanSdk
$env:Vulkan_LIBRARY = Join-Path $vulkanSdk 'Lib\vulkan-1.lib'
$env:Vulkan_INCLUDE_DIR = Join-Path $vulkanSdk 'Include'
$env:PATH = "$(Join-Path $vulkanSdk 'Bin');$env:PATH"
Write-Host "VULKAN_SDK = $vulkanSdk"

# --- Frontend ----------------------------------------------------------------
# The Rust build embeds frontend\out, so it has to exist before any cargo build.
if (-not $SkipFrontend) {
  Push-Location $frontend
  try { Invoke-Checked 'Next.js build' { node 'node_modules\next\dist\bin\next' build } } finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath (Join-Path $frontend 'out\index.html'))) { throw 'frontend\out is missing; run without -SkipFrontend' }

Push-Location $repo
try {
  # --- Local summary model sidecar (CPU; as the base project's CI builds it) ---
  Invoke-Checked 'llama-helper build' { cargo build --release -p llama-helper }
  $binaries = Join-Path $tauri 'binaries'
  New-Item -ItemType Directory -Force -Path $binaries | Out-Null
  Copy-Item 'target\release\llama-helper.exe' (Join-Path $binaries 'llama-helper-x86_64-pc-windows-msvc.exe') -Force

  # --- VC++ runtime, installed by the installer when it is missing -------------
  # The installer stops on a PC without this runtime unless the package is
  # bundled. Microsoft replaces the download in place, so a pinned checksum goes
  # stale; the file is accepted when Windows confirms Microsoft's signature.
  $deps = Join-Path $tauri 'runtime-deps'
  New-Item -ItemType Directory -Force -Path $deps | Out-Null
  $vc = Join-Path $deps 'vc_redist.x64.exe'
  if (-not (Test-Path -LiteralPath $vc)) {
    Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $vc -UseBasicParsing
  }
  $vcSignature = Get-AuthenticodeSignature -LiteralPath $vc
  if ($vcSignature.Status -ne 'Valid' -or $vcSignature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
    Remove-Item -LiteralPath $vc -Force
    throw "vc_redist.x64.exe is not validly signed by Microsoft (status: $($vcSignature.Status))"
  }
  Write-Host "VC++ redistributable verified: $($vcSignature.SignerCertificate.Subject)"

  # --- App binary (Vulkan) -----------------------------------------------------
  Invoke-Checked 'App build (Vulkan)' { cargo build --release -p meetily --bin meetily --no-default-features --features 'custom-protocol,vulkan' }
  $appBinary = Join-Path $repo 'target\release\meetily.exe'
  if (-not (Test-Path -LiteralPath $appBinary)) { throw "App binary missing: $appBinary" }

  # --- DirectML beside the program, as the base's release installer has it -----
  # The app's build script has just staged and verified ONNX Runtime under
  # binaries\onnxruntime. The installer copies runtime-deps\DirectML.dll next to
  # the executable ("Common local AI runtime"); without it the installer only
  # warns, and the app loads the copy under binaries\onnxruntime.
  $directMl = Join-Path $tauri 'binaries\onnxruntime\DirectML.dll'
  if (-not (Test-Path -LiteralPath $directMl)) { throw "The app build did not stage ONNX Runtime: $directMl" }
  Copy-Item -LiteralPath $directMl -Destination (Join-Path $deps 'DirectML.dll') -Force

  # --- DLLs the program loads at start (for the build log) ---------------------
  # vulkan-1.dll in this list means the app starts only where a graphics driver
  # provides the Vulkan loader.
  $dumpbin = Get-Command 'dumpbin.exe' -ErrorAction SilentlyContinue
  if ($dumpbin) {
    Write-Host ""
    Write-Host "=== DLL imports of the app ==="
    & $dumpbin.Source /nologo /dependents $appBinary | Where-Object { $_ -match '\.dll\s*$' } | ForEach-Object { Write-Host "  $($_.Trim())" }
  }

  # --- Installer progress plugin ----------------------------------------------
  & (Join-Path $PSScriptRoot 'build-nsis-progress-plugin.ps1') -ProgressMainBinary $appBinary -MainBinaryName $mainBinary
  if ($LASTEXITCODE -ne 0) { throw 'NSIS overall-progress plugin build failed' }
} finally { Pop-Location }

# --- Installer -----------------------------------------------------------------
Push-Location $frontend
try {
  Invoke-Checked 'Tauri bundle (NSIS)' {
    node 'node_modules\@tauri-apps\cli\tauri.js' build --bundles nsis --config 'src-tauri\tauri.homebrew.conf.json' -- --no-default-features --features 'custom-protocol,vulkan'
  }
} finally { Pop-Location }

$bundle = Join-Path $repo 'target\release\bundle\nsis'
$installer = Get-ChildItem -LiteralPath $bundle -Filter '*-setup.exe' -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) { throw "No installer was produced in $bundle" }
if ($installer.Length -lt (20 * 1024 * 1024)) { throw "The installer is implausibly small: $($installer.Length) bytes" }

$dist = Join-Path $repo 'dist-homebrew'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$outputName = "Meetily-Homebrew-$version-x64-vulkan-setup.exe"
$output = Join-Path $dist $outputName
Copy-Item -LiteralPath $installer.FullName -Destination $output -Force
$hash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS.txt'), "$hash  $outputName`n")
Write-Host ""
Write-Host "Unsigned installer of '$product' $version (Vulkan): $output"
Write-Host "SHA-256: $hash"
