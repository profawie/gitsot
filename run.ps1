<#
.SYNOPSIS
  Build (if needed) and start Gitsot on Windows.

.EXAMPLE
  .\run.ps1                       # open the last repository
  .\run.ps1 C:\work\my-project    # open a specific repository
  .\run.ps1 -Dev                  # use a debug build
#>
param(
    [switch]$Dev,
    [string]$Repo
)

$ErrorActionPreference = 'Stop'

$repoPath = $null
if ($Repo) {
    if (-not (Test-Path $Repo -PathType Container)) {
        Write-Host "error: '$Repo' is not a folder" -ForegroundColor Red
        exit 1
    }
    $repoPath = (Resolve-Path $Repo).Path
}

Set-Location -Path $PSScriptRoot
& "$PSScriptRoot\build.ps1" -Dev:$Dev
if ($LASTEXITCODE -ne 0 -and $null -ne $LASTEXITCODE) { exit $LASTEXITCODE }

$exe = Join-Path $PSScriptRoot 'dist\gitsot.exe'
if ($repoPath) {
    Start-Process -FilePath $exe -ArgumentList "`"$repoPath`""
} else {
    Start-Process -FilePath $exe
}
