# Quality gate for the browser workspace.

# Usage:  powershell -ExecutionPolicy Bypass -File tools/check.ps1
# Probe:  powershell -ExecutionPolicy Bypass -File tools/check.ps1 -ToolchainOnly
# Runs:   fmt check, workspace build, all tests, clippy (no warnings).

param([switch]$ToolchainOnly)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# On Windows the build needs LLVM (libclang for bindgen) and Python on
# the PATH, plus LIBCLANG_PATH pointing at an LLVM bin directory.

function Add-PathEntry([string]$directory) {
    if (-not $directory) {
        return
    }

    $resolved = (Resolve-Path -LiteralPath $directory).Path
    $pathEntries = @($env:Path -split [IO.Path]::PathSeparator)
    if ($pathEntries -notcontains $resolved) {
        $env:Path = "$resolved$([IO.Path]::PathSeparator)$env:Path"
    }
}

function Test-LibClangDirectory([string]$directory) {
    return $directory -and
        (Test-Path -LiteralPath (Join-Path $directory "libclang.dll") -PathType Leaf)
}

function Get-VisualStudioInstallations {
    $installations = @()
    $programFilesX86 = [Environment]::GetFolderPath(
        [Environment+SpecialFolder]::ProgramFilesX86
    )

    if ($programFilesX86) {
        $vswhere = Join-Path $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
        if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
            $vswhereResults = & $vswhere -all -products * -property installationPath 2>$null
            if ($LASTEXITCODE -eq 0) {
                $installations += @($vswhereResults | Where-Object { $_ })
            }
        }
    }

    $programFilesRoots = @(
        [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles),
        $programFilesX86
    ) | Where-Object { $_ } | Select-Object -Unique

    foreach ($programFilesRoot in $programFilesRoots) {
        $visualStudioRoot = Join-Path $programFilesRoot "Microsoft Visual Studio"
        if (-not (Test-Path -LiteralPath $visualStudioRoot -PathType Container)) {
            continue
        }

        foreach ($versionDirectory in @(Get-ChildItem -LiteralPath $visualStudioRoot -Directory -ErrorAction SilentlyContinue)) {
            foreach ($editionDirectory in @(Get-ChildItem -LiteralPath $versionDirectory.FullName -Directory -ErrorAction SilentlyContinue)) {
                $installations += $editionDirectory.FullName
            }
        }
    }

    return @($installations | Select-Object -Unique)
}

$programFilesRoots = @(
    [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles),
    [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFilesX86)
) | Where-Object { $_ } | Select-Object -Unique

$llvmCandidates = @()
if (Test-LibClangDirectory $env:LIBCLANG_PATH) {
    $llvmCandidates += $env:LIBCLANG_PATH
}
foreach ($programFilesRoot in $programFilesRoots) {
    $llvmCandidates += Join-Path $programFilesRoot "LLVM\bin"
}
foreach ($installation in @(Get-VisualStudioInstallations)) {
    $llvmCandidates += Join-Path $installation "VC\Tools\Llvm\x64\bin"
    $llvmCandidates += Join-Path $installation "VC\Tools\Llvm\bin"
}

$llvmBins = $llvmCandidates |
    Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Container) } |
    ForEach-Object { (Resolve-Path -LiteralPath $_).Path } |
    Select-Object -Unique
$llvmBin = $llvmBins | Select-Object -First 1
$libClangBin = $llvmBins |
    Where-Object { Test-LibClangDirectory $_ } |
    Select-Object -First 1

if ($llvmBin) {
    Add-PathEntry $llvmBin
    Write-Host "LLVM tools: $llvmBin" -ForegroundColor DarkGray
}
else {
    Write-Warning "No LLVM bin directory was found."
}

if ($libClangBin) {
    Add-PathEntry $libClangBin
    $env:LIBCLANG_PATH = $libClangBin
    Write-Host "libclang: $libClangBin" -ForegroundColor DarkGray
}
else {
    Write-Warning "No LLVM bin directory containing libclang.dll was found."
}

$pythonCandidates = @()
$pythonCommands = @(Get-Command python.exe, python3.exe -CommandType Application -ErrorAction SilentlyContinue)
foreach ($pythonCommand in $pythonCommands) {
    if ($pythonCommand.Source -and $pythonCommand.Source -notmatch "\\WindowsApps\\") {
        $pythonCandidates += Split-Path -Parent $pythonCommand.Source
    }
}

$pythonRoots = @()
if ($env:LOCALAPPDATA) {
    $pythonRoots += Join-Path $env:LOCALAPPDATA "Programs\Python"
}
$pythonRoots += $programFilesRoots
if ($env:SystemDrive) {
    $pythonRoots += "$($env:SystemDrive)\"
}

foreach ($pythonRoot in @($pythonRoots | Select-Object -Unique)) {
    if (-not (Test-Path -LiteralPath $pythonRoot -PathType Container)) {
        continue
    }

    $pythonCandidates += @(
        Get-ChildItem -LiteralPath $pythonRoot -Directory -Filter "Python*" -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending |
            Select-Object -ExpandProperty FullName
    )
}

$pythonDir = $pythonCandidates |
    Where-Object {
        $_ -and (Test-Path -LiteralPath (Join-Path $_ "python.exe") -PathType Leaf)
    } |
    ForEach-Object { (Resolve-Path -LiteralPath $_).Path } |
    Select-Object -Unique |
    Select-Object -First 1

if ($pythonDir) {
    Add-PathEntry $pythonDir
    Write-Host "Python: $pythonDir" -ForegroundColor DarkGray
}
else {
    Write-Warning "No usable Python installation was found."
}

if ($ToolchainOnly) {
    if ($llvmBins) {
        Write-Host "Detected LLVM bin directories:" -ForegroundColor DarkGray
        $llvmBins | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
    }
    if (-not $llvmBin) {
        throw "LLVM discovery failed."
    }
    if (-not $libClangBin) {
        throw "libclang discovery failed."
    }
    if (-not $pythonDir) {
        throw "Python discovery failed."
    }

    Write-Host "Toolchain discovery passed." -ForegroundColor Green
    return
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
