# Quality gate for the browser workspace.

# Usage:  powershell -ExecutionPolicy Bypass -File tools/check.ps1
# Runs:   fmt check, workspace build, all tests, clippy (no warnings).

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# On Windows the build needs LLVM (libclang for bindgen) and Python on
# the PATH, plus LIBCLANG_PATH pointing at an LLVM bin directory. Only
# apply these when the directories actually exist so the script also
# works on machines without them.
$llvmBins = @(
    "C:\Program Files\LLVM\bin",
    "C:\Program Files\Microsoft Visual Studio\18\2022\Community\VC\Tools\LLVM\x64\bin"
)
$pythonDirs = @(
    "C:\Users\user\AppData\Local\Programs\Python\Python312",
    "C:\Python312"
)
foreach ($bin in $llvmBins) {
    if (Test-Path -LiteralPath $bin) {
        $env:Path = "$bin;$env:Path"
        if (-not $env:LIBCLANG_PATH) {
            $env:LIBCLANG_PATH = $bin
        }
    }
}
foreach ($dir in $pythonDirs) {
    if (Test-Path -LiteralPath $dir) {
        $env:Path = "$dir;$env:Path"
    }
}

function Invoke-Step([string]$name, [string[]]$cmd) {
    Write-Host "== $name" -ForegroundColor Cyan
    Push-Location $root
    try {
        & $cmd[0] $cmd[1..($cmd.Count - 1)]
        if ($LASTEXITCODE -ne 0) {
            throw "$name failed (exit $LASTEXITCODE)"
        }
    }
    finally {
        Pop-Location
    }
}

Write-Host "Quality gate: $root" -ForegroundColor Green

Invoke-Step "cargo fmt --check" @("cargo", "fmt", "--all", "--check")
Invoke-Step "cargo build (workspace)" @("cargo", "build", "--workspace")
Invoke-Step "cargo test --workspace" @("cargo", "test", "--workspace")
Invoke-Step "cargo clippy (workspace, all targets)" @("cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings")

Write-Host "All checks passed." -ForegroundColor Green