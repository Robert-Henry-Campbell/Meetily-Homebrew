# Puts LLVM 18 into a folder: libclang, clang and clang's own headers. The
# build generates the whisper.cpp and llama.cpp bindings with libclang, and the
# base project asks for LLVM 18 for them.
#
# The base's bootstrap-llvm18.ps1 fetches a .tar.xz with "gh release download"
# and unpacks it with Windows tar. On a GitHub build machine (Windows Server
# 2022) that sat without output until the job's three-hour limit: neither tool
# has a time limit there, and that tar cannot read xz by itself. This script
# takes the official Windows installer instead, limits the download in time,
# checks its SHA-256, and unpacks the three parts the build needs with 7-Zip.
#
# Output: the folder, as the last value. On a GitHub build machine it is also
# passed to later steps as MEETILY_LLVM_DIR, which build-homebrew-windows.ps1
# reads.
param(
  [string]$Destination
)

$ErrorActionPreference = 'Stop'
$frontend = Split-Path $PSScriptRoot -Parent
$repo = Split-Path $frontend -Parent
if (-not $Destination) { $Destination = Join-Path $repo '.build-tools\llvm-18.1.8-win64' }

$version = '18.1.8'
$url = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$version/LLVM-$version-win64.exe"
$expectedSha256 = '94AF030060D88CC17E9F00EF1663EBDC1126B35E16BEBDFA1E807984B70ABD8F'
$libclang = Join-Path $Destination 'bin\libclang.dll'
$clang = Join-Path $Destination 'bin\clang.exe'
$header = Join-Path $Destination 'lib\clang\18\include\stddef.h'

function Test-Llvm18 {
  foreach ($required in @($libclang, $clang, $header)) {
    if (-not (Test-Path -LiteralPath $required)) { return $false }
  }
  $reported = & $clang --version | Select-Object -First 1
  return ($reported -match 'version 18\.')
}

if (Test-Llvm18) {
  Write-Host "LLVM 18 is already present: $Destination"
} else {
  $sevenZip = Get-Command '7z.exe' -ErrorAction SilentlyContinue
  $sevenZipPath = if ($sevenZip) { $sevenZip.Source } else { Join-Path $env:ProgramFiles '7-Zip\7z.exe' }
  if (-not (Test-Path -LiteralPath $sevenZipPath)) { throw '7-Zip (7z.exe) is needed to unpack the LLVM installer' }

  New-Item -ItemType Directory -Force -Path $Destination | Out-Null
  $installer = Join-Path ([System.IO.Path]::GetTempPath()) "LLVM-$version-win64.exe"
  Write-Host "Downloading LLVM $version (374 MB): $url"
  & curl.exe --fail --location --silent --show-error --retry 3 --retry-delay 5 --connect-timeout 30 --max-time 900 --output $installer $url
  if ($LASTEXITCODE -ne 0) { throw "The LLVM download failed (curl exit code $LASTEXITCODE)" }
  $actualSha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash
  if ($actualSha256 -ne $expectedSha256) { throw "The LLVM installer has an unexpected SHA-256: $actualSha256" }

  # The installer is an NSIS archive, which 7-Zip reads. The rest of it (about
  # 2.5 GB of tools) is not needed for generating bindings.
  Write-Host 'Unpacking libclang, clang and the clang headers'
  $unpack = @('x', $installer, "-o$Destination", 'bin\libclang.dll', 'bin\clang.exe', 'lib\clang', '-y', '-bso0', '-bsp0')
  & $sevenZipPath @unpack
  if ($LASTEXITCODE -ne 0) { throw "7-Zip could not unpack the LLVM installer (exit code $LASTEXITCODE)" }
  Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue
  if (-not (Test-Llvm18)) { throw "LLVM 18 is incomplete after unpacking: $Destination" }
}

$reportedVersion = & $clang --version | Select-Object -First 1
Write-Host "LLVM ready: $reportedVersion"
if ($env:GITHUB_ENV) {
  "MEETILY_LLVM_DIR=$Destination" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
}
$Destination
