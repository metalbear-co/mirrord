Set-StrictMode -Version Latest

function Invoke-GoBuild {
    param(
        [string]$OutputName,
        [string]$DirectoryName,
        [string]$GoToolchain
    )

    $previousToolchain = $env:GOTOOLCHAIN
    if ($GoToolchain) {
        $env:GOTOOLCHAIN = $GoToolchain
    }

    try {
        & go build -o $OutputName | Out-Null
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            throw "go build exited with code $exitCode in directory $DirectoryName for output $OutputName using toolchain $GoToolchain"
        }
    } finally {
        if ($GoToolchain) {
            if ($null -eq $previousToolchain) {
                Remove-Item Env:GOTOOLCHAIN -ErrorAction SilentlyContinue
            } else {
                $env:GOTOOLCHAIN = $previousToolchain
            }
        }
    }
}

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

Write-Host 'Installing dependencies: nodejs, go, python3, curl'
choco install -y nodejs go python3 curl

Write-Host 'Installing Python packages required by tests'
python -m pip install --upgrade pip
python -m pip install uvicorn fastapi flask

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$testsDir = Join-Path $repoRoot 'tests'

if (-not (Test-Path $testsDir)) {
    throw "Tests directory not found at path $testsDir"
}

$goTargets = @(
    @{ Output = '23.go_test_app.exe'; Toolchain = 'go1.23.12' },
    @{ Output = '24.go_test_app.exe'; Toolchain = 'go1.24.7' },
    @{ Output = '25.go_test_app.exe'; Toolchain = 'go1.25.1' }
)

Push-Location $testsDir
try {
    $goDirs = Get-ChildItem -Directory -Filter 'go-e2e-*'
    if ($goDirs.Count -eq 0) {
        Write-Warning 'No go-e2e-* directories found; skipping Go build steps.'
    }

    foreach ($dir in $goDirs) {
        Write-Host "Building in $($dir.Name)"
        Push-Location $dir.FullName
        try {
            foreach ($target in $goTargets) {
                try {
                    Invoke-GoBuild -OutputName $target.Output -DirectoryName $dir.Name -GoToolchain $target.Toolchain
                } catch {
                    Write-Warning "Failed to build $($target.Output) in $($dir.Name) using $($target.Toolchain): $($_.Exception.Message)"
                    break
                }
            }
        } finally {
            Pop-Location
        }
    }
} finally {
    Pop-Location
}

$buildScripts = @(
    @{ Path = Join-Path $repoRoot 'scripts/build_c_apps.sh'; Description = 'C test applications'; },
    @{ Path = Join-Path $repoRoot 'scripts/build_go_apps.sh'; Description = 'Go test applications'; }
)

$bashCommand = Get-Command bash -ErrorAction SilentlyContinue
$requiresWslPath = $false
if ($bashCommand -and $bashCommand.Source -match 'system32\\bash.exe') {
    $requiresWslPath = $true
    $wslCommand = Get-Command wsl -ErrorAction SilentlyContinue
    if (-not $wslCommand) {
        Write-Error "WSL bash detected but Windows Subsystem for Linux is not enabled. Please follow https://learn.microsoft.com/windows/wsl/install to install WSL."
        exit 1
    }
}

foreach ($buildScript in $buildScripts) {
    $scriptPath = $buildScript.Path
    if (-not (Test-Path $scriptPath)) {
        Write-Warning "Skipping $($buildScript.Description) build; script not found at $scriptPath"
        continue
    }

    if (-not $bashCommand) {
        Write-Warning "Skipping $($buildScript.Description) build; 'bash' command not available"
        continue
    }

    $posixPath = $scriptPath -replace '\\', '/'
    if ($requiresWslPath) {
        $wslOutput = & wsl wslpath -a "$scriptPath"
        if ($LASTEXITCODE -eq 0 -and $wslOutput) {
            $posixPath = $wslOutput.Trim()
        } else {
            Write-Warning "Failed to convert $scriptPath to WSL path, using POSIX path $posixPath"
        }
    }

    Write-Host "Running $($buildScript.Description) build script..."
    & bash $posixPath
    if ($LASTEXITCODE -ne 0) {
        throw "Script $scriptPath failed with exit code $LASTEXITCODE"
    }
}

Write-Host 'Finished building test apps.'