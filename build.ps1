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

  Output: dist\gitsot, a folder with every file gitsot needs:
    gitsot.exe   the small launcher you start
    gitsot.dll   the application itself (UI, Git, terminal)
    + any MinGW runtime DLLs the two files import (usually none, because
      Rust links them statically)
  Copy the whole folder to another PC and run gitsot.exe from there.

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

$Version = (Select-String -Path 'Cargo.toml' -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value

Write-Host "==> Building gitsot $Version ($Target, $BuildProfile)"
$cargoArgs = @("+$Toolchain", 'build', '--workspace', '--target', $Target)
if (-not $Dev) { $cargoArgs += '--release' }
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { Fail 'build failed' }

# Assemble the release folder.
$Built = "target\$Target\$BuildProfile"
$Out   = 'dist\gitsot'
if (Test-Path $Out) {
    try {
        Remove-Item -Recurse -Force $Out
    } catch {
        Fail "cannot replace $Out. Close gitsot if it is running from that folder and try again."
    }
}
New-Item -ItemType Directory -Force -Path $Out | Out-Null
Copy-Item "$Built\gitsot.exe" "$Out\gitsot.exe" -Force
Copy-Item "$Built\gitsot.dll" "$Out\gitsot.dll" -Force

# Copy the MinGW runtime DLLs (libgcc, libwinpthread, ...) that a file
# imports, if any. DLLs that are not in the MinGW folder are part of Windows.
$MingwBin = Split-Path -Parent (Get-Command gcc).Source
$Objdump  = Join-Path $MingwBin 'objdump.exe'
function Copy-RuntimeDlls([string]$File) {
    $imports = & $Objdump -p $File | Select-String -Pattern '^\s*DLL Name: (.+)$' |
        ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() } | Sort-Object -Unique
    foreach ($dll in $imports) {
        $dest = Join-Path $Out $dll
        if (Test-Path $dest) { continue }
        $src = Join-Path $MingwBin $dll
        if (-not (Test-Path $src)) { continue }
        Write-Host "    + $dll"
        Copy-Item $src $dest -Force
        Copy-RuntimeDlls $dest
    }
}
Write-Host '==> Collecting runtime DLLs'
if (Test-Path $Objdump) {
    Copy-RuntimeDlls "$Out\gitsot.exe"
    Copy-RuntimeDlls "$Out\gitsot.dll"
} else {
    Write-Host "    objdump.exe not found in $MingwBin, skipping (install mingw-w64-x86_64-binutils)" -ForegroundColor Yellow
}

Write-Host "==> Done: $Out" -ForegroundColor Green
Get-ChildItem $Out | ForEach-Object { Write-Host "    $($_.Name)" }
