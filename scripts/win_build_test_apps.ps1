Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'win_build_test_apps_utils.ps1')

$ErrorActionPreference = 'Stop'

Write-Host 'Ensuring Chocolatey is installed...'
if (-not (Get-Command choco -ErrorAction SilentlyContinue)) {
    Write-Host 'Chocolatey not found, installing...'
    Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass -Force
    $installScript = Invoke-WebRequest -Uri 'https://community.chocolatey.org/install.ps1' -UseBasicParsing
    Invoke-Expression $installScript.Content
} else {
    Write-Host 'Chocolatey already installed.'
}

Write-Host 'Installing dependencies: nodejs, go, python3, curl, mingw, cmake, nasm, llvm'
function Install-NasmPortable {
    param(
        [string]$DestDir = "C:\nasm",
        [string]$Url = "https://storage.googleapis.com/grpc-build-helper/nasm-2.15.05/nasm.exe"
    )

    Write-Host "Installing nasm to $DestDir (portable download)"
    New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
    $destExe = Join-Path $DestDir "nasm.exe"
    Invoke-WebRequest -Uri $Url -OutFile $destExe -UseBasicParsing
    if (-not (Test-Path $destExe)) {
        throw "Failed to download nasm from $Url"
    }

    # Prepend to PATH for current process; caller can persist if desired.
    $env:PATH = "$DestDir;$env:PATH"
    Write-Host "nasm installed at $destExe"
}

# Install everything except nasm via Chocolatey to reduce privilege needs; fetch nasm manually.
choco install -y nodejs go python3 curl mingw cmake llvm
if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) {
    Install-NasmPortable
} else {
    Write-Host 'nasm already installed.'
}

Write-Host 'Preparing Python virtual environment for test dependencies'
$venvRoot = Join-Path $env:TEMP "mirrord-test-venv"
if (Test-Path $venvRoot) {
    Remove-Item -Recurse -Force $venvRoot
}
python -m pip install --upgrade pip virtualenv
python -m venv $venvRoot
$venvActivate = Join-Path $venvRoot "Scripts\Activate.ps1"

try {
    Write-Host "Activating virtual environment at $venvRoot"
    . $venvActivate

    Write-Host 'Installing Python packages required by tests'
    python -m pip install --upgrade pip
    python -m pip install uvicorn fastapi flask
    Write-Host 'Installed Python packages:'
    python -c "import pkgutil, sys; [print(module.module_finder.path) for module in pkgutil.iter_modules()]"
} finally {
    Write-Host "Deactivating and removing virtual environment"
    if (Get-Command deactivate -ErrorAction SilentlyContinue) {
        deactivate
    }
    if (Test-Path $venvRoot) {
        Remove-Item -Recurse -Force $venvRoot
    }
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$testsDir = Join-Path $repoRoot 'tests'

if (-not (Test-Path $testsDir)) {
    throw "Tests directory not found at path $testsDir"
}

# build Rust test binaries used by mirrord-tests targetless fixtures
Build-RustApps -RepoRoot $repoRoot

# builds Windows go-e2e test binaries with specific toolchains
Build-GoE2EApps -TestsDir $testsDir

# mirrors scripts/build_go_apps.sh (builds generic Go test binaries across the repo)
Build-RepoGoApps -RepoRoot $repoRoot -OutputPrefix '25'

Write-Host 'Finished building test apps.'