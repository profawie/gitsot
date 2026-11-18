<#
.SYNOPSIS
  Build Gitsot for Windows using the GNU toolchain (x86_64-pc-windows-gnu).

.DESCRIPTION
  Requirements:
    * Rust via rustup (https://rustup.rs)
    * MinGW-w64 gcc on PATH, used to compile libgit2. The easiest way:
        winget install MSYS2.MSYS2
        C:\msys64\usr\bin\pacman.exe -S --needed mingw-w64-x86_64-gcc
      This script adds C:\msys64\mingw64\bin to PATH automatically if it exists.

  The GNU Rust toolchain is installed automatically if missing.
  Output: dist\gitsot.exe

.EXAMPLE
  .\build.ps1          # release build
  .\build.ps1 -Dev     # debug build
#>
param(
    [switch]$Dev
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

$Target    = 'x86_64-pc-windows-gnu'
$Toolchain = "stable-$Target"
$BuildProfile   = if ($Dev) { 'debug' } else { 'release' }

function Fail($msg) {
    Write-Host "error: $msg" -ForegroundColor Red
    exit 1
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail 'cargo not found. Install Rust from https://rustup.rs and reopen the terminal.'
}

# Make a MinGW-w64 gcc available (MSYS2 or other common install locations).
if (-not (Get-Command gcc -ErrorAction SilentlyContinue)) {
    $candidates = @(
        'C:\msys64\mingw64\bin',
        "$env:USERPROFILE\scoop\apps\mingw\current\bin",
        'C:\ProgramData\mingw64\mingw64\bin',
        'C:\mingw64\bin'
    )
    foreach ($dir in $candidates) {
        if (Test-Path (Join-Path $dir 'gcc.exe')) {
            Write-Host "==> Using MinGW from $dir"
            $env:PATH = "$dir;$env:PATH"
            break
        }
    }
}
if (-not (Get-Command gcc -ErrorAction SilentlyContinue)) {
    Fail @'
MinGW-w64 gcc not found. Install it, for example:
    winget install MSYS2.MSYS2
    C:\msys64\usr\bin\pacman.exe -S --needed mingw-w64-x86_64-gcc
then run this script again.
'@
}

# Make sure the GNU toolchain is installed.
$installed = (& rustup toolchain list) -join "`n"
if ($installed -notmatch [regex]::Escape($Toolchain)) {
    Write-Host "==> Installing Rust toolchain $Toolchain"
    & rustup toolchain install $Toolchain --profile minimal
    if ($LASTEXITCODE -ne 0) { Fail "could not install $Toolchain" }
}

Write-Host "==> Building Gitsot ($Target, $BuildProfile)"
$cargoArgs = @("+$Toolchain", 'build', '--target', $Target)
if (-not $Dev) { $cargoArgs += '--release' }
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { Fail 'build failed' }

New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item "target\$Target\$BuildProfile\gitsot.exe" 'dist\gitsot.exe' -Force
Write-Host "==> Done: dist\gitsot.exe" -ForegroundColor Green
