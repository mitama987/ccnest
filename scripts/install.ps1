# Build ccnest in release mode and copy the binary onto the user's PATH.
#
# Usage (from repo root):
#     pwsh .\scripts\install.ps1
#
# By default this copies to $HOME\.local\bin (where claude.exe already lives).
# Override with $env:CCNEST_INSTALL_DIR before invoking.
#
# Tip: if you have rustup, `cargo install --path .` is the idiomatic alternative
# and drops ccnest.exe into $HOME\.cargo\bin (already on PATH via rustup).

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    Write-Host "==> cargo build --release" -ForegroundColor Cyan
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

    $target = Join-Path $repoRoot 'target\release\ccnest.exe'
    if (-not (Test-Path $target)) {
        throw "expected binary not found: $target"
    }

    $installDir = if ($env:CCNEST_INSTALL_DIR) {
        $env:CCNEST_INSTALL_DIR
    } else {
        Join-Path $HOME '.local\bin'
    }
    if (-not (Test-Path $installDir)) {
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    }

    $dest = Join-Path $installDir 'ccnest.exe'
    Copy-Item $target $dest -Force
    Write-Host "==> installed: $dest" -ForegroundColor Green

    $pathDirs = ($env:PATH -split ';')
    if ($pathDirs -notcontains $installDir) {
        Write-Warning "$installDir is not on PATH. Add it, or re-run with CCNEST_INSTALL_DIR set to a dir that is."
    } else {
        Write-Host "==> PATH is configured. Run 'ccnest' from any directory." -ForegroundColor Green
    }
}
finally {
    Pop-Location
}
