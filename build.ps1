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

  Output: dist, a folder with every file gitsot needs:
    gitsot.exe          the small launcher you start
    gitsot.dll          the application: window and the whole UI
    gitsot_core.dll     Git, identities and the history graph
    gitsot_term.dll     the built-in terminal
    gitsot_forge.dll    pull requests on the Git host
    gitsot_shared.dll   third-party libraries the gitsot DLLs share
    std-<hash>.dll      the Rust standard library, shared by all of them
    libgcc_s_seh-1.dll  the MinGW runtime (+ any other MinGW DLL imported)
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
$Out   = 'dist'
if (Test-Path $Out) {
    try {
        Remove-Item -Recurse -Force $Out
    } catch {
        Fail "cannot replace $Out. Close gitsot if it is running from that folder and try again."
    }
}
New-Item -ItemType Directory -Force -Path $Out | Out-Null

# Folders a DLL may come from: this build, the Rust standard library
# (std-<hash>.dll) and MinGW (libgcc_s_seh-1.dll and friends).
$RustLib  = (& rustup run $Toolchain rustc --print target-libdir --target $Target).Trim()
$RustBin  = Join-Path (& rustup run $Toolchain rustc --print sysroot).Trim() 'bin'
$MingwBin = Split-Path -Parent (Get-Command gcc).Source
$DllDirs  = @($Built, $RustLib, $RustBin, $MingwBin)
$Objdump  = Join-Path $MingwBin 'objdump.exe'
if (-not (Test-Path $Objdump)) {
    Fail "objdump.exe not found in $MingwBin. Install it: C:\msys64\usr\bin\pacman.exe -S --needed mingw-w64-x86_64-binutils"
}

# Copy a file into dist, then every DLL it imports that is found in $DllDirs,
# and so on for those. DLLs that are found nowhere are part of Windows.
function Copy-WithDlls([string]$File) {
    $name = Split-Path -Leaf $File
    if (-not (Test-Path (Join-Path $Out $name))) { Copy-Item $File (Join-Path $Out $name) -Force }
    $imports = & $Objdump -p $File | Select-String -Pattern '^\s*DLL Name: (.+)$' |
        ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() } | Sort-Object -Unique
    foreach ($dll in $imports) {
        if (Test-Path (Join-Path $Out $dll)) { continue }
        foreach ($dir in $DllDirs) {
            $src = Join-Path $dir $dll
            if (Test-Path $src) {
                Copy-WithDlls $src
                break
            }
        }
    }
}
Write-Host '==> Collecting DLLs'
Copy-WithDlls "$Built\gitsot.exe"
# The launcher loads gitsot.dll at run time, so it is not in its imports.
Copy-WithDlls "$Built\gitsot.dll"

Write-Host "==> Done: $Out\" -ForegroundColor Green
Get-ChildItem $Out | Sort-Object Name | ForEach-Object {
    Write-Host ('    {0,-28} {1,8:N0} KB' -f $_.Name, [math]::Ceiling($_.Length / 1KB))
}
