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
    }
    finally {
        if ($GoToolchain) {
            if ($null -eq $previousToolchain) {
                Remove-Item Env:GOTOOLCHAIN -ErrorAction SilentlyContinue
            } else {
                $env:GOTOOLCHAIN = $previousToolchain
            }
        }
    }
}

function Convert-ToWslPath {
    param(
        [string]$WindowsPath
    )

    if ($WindowsPath -match '^[A-Za-z]:\\') {
        $driveLetter = $WindowsPath.Substring(0, 1).ToLower()
        $relativePath = $WindowsPath.Substring(2) -replace '\\', '/'
        return "/mnt/$driveLetter$relativePath"
    }

    return $WindowsPath -replace '\\', '/'
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

# Refresh environment variables to pick up newly installed packages
$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")

Write-Host 'Installing Python packages required by tests'
# Try different Python command names (Chocolatey may install as python, python3, or py)
$pythonCmd = $null
if (Get-Command python -ErrorAction SilentlyContinue) {
    $pythonCmd = "python"
} elseif (Get-Command python3 -ErrorAction SilentlyContinue) {
    $pythonCmd = "python3"
} elseif (Get-Command py -ErrorAction SilentlyContinue) {
    $pythonCmd = "py"
} else {
    # Try to find Python in common Chocolatey installation locations
    $chocoPythonPath = "C:\ProgramData\chocolatey\lib\python3\tools\python.exe"
    if (Test-Path $chocoPythonPath) {
        $pythonCmd = $chocoPythonPath
    } else {
        throw "Python not found. Please ensure python3 was installed via Chocolatey."
    }
}

Write-Host "Using Python: $pythonCmd"
& $pythonCmd -m pip install --upgrade pip
& $pythonCmd -m pip install uvicorn fastapi flask

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
    @{ Path = Join-Path $repoRoot 'scripts/build_c_apps.sh'; Description = 'C test applications'; Arguments = @('out.c_test_app') },
    @{ Path = Join-Path $repoRoot 'scripts/build_go_apps.sh'; Description = 'Go test applications'; Arguments = @('25') }
)

$wslCommand = Get-Command wsl -ErrorAction SilentlyContinue
if (-not $wslCommand) {
    throw "WSL is required to build native test applications. Follow the setup guide at https://learn.microsoft.com/windows/wsl/install"
}

Write-Host 'Ensuring Go is available inside WSL...'
$ensureGoScript = @'
set -euo pipefail
install_dir="$HOME/.mirrord-go"
if command -v go >/dev/null 2>&1; then
    exit 0
fi
if [ -x "$install_dir/current/bin/go" ]; then
    exit 0
fi
GO_VERSION="go1.25.1"
ARCHIVE="${GO_VERSION}.linux-amd64.tar.gz"
TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT
cd "$TMP_DIR"
if command -v curl >/dev/null 2>&1; then
    curl -fsSLo go.tgz "https://go.dev/dl/$ARCHIVE"
elif command -v wget >/dev/null 2>&1; then
    wget -qO go.tgz "https://go.dev/dl/$ARCHIVE"
else
    echo "WSL is missing curl or wget; cannot install Go." >&2
    exit 1
fi
tar -xzf go.tgz
mkdir -p "$install_dir"
rm -rf "$install_dir/$GO_VERSION"
mv go "$install_dir/$GO_VERSION"
ln -sfn "$install_dir/$GO_VERSION" "$install_dir/current"
'@

$ensureGoScript = $ensureGoScript -replace "`r`n", "`n"
$tempGoScript = [System.IO.Path]::GetTempFileName()
try {
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($tempGoScript, $ensureGoScript, $utf8NoBom)
    $tempGoScriptWsl = Convert-ToWslPath -WindowsPath $tempGoScript
    & wsl --exec /bin/bash $tempGoScriptWsl
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to provision Go inside WSL'
    }
} finally {
    Remove-Item $tempGoScript -ErrorAction SilentlyContinue
}

$wslHome = (& wsl --exec /bin/bash -lc 'printf %s "$HOME"')
if ([string]::IsNullOrWhiteSpace($wslHome)) {
    throw 'Failed to resolve WSL home directory'
}

$wslBasePath = '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin'
$goPathPrefix = "PATH=$wslHome/.mirrord-go/current/bin:$wslBasePath"

foreach ($buildScript in $buildScripts) {
    $scriptPath = $buildScript.Path
    if (-not (Test-Path $scriptPath)) {
        Write-Warning "Skipping $($buildScript.Description) build; script not found at $scriptPath"
        continue
    }

    $arguments = @()
    if ($buildScript.ContainsKey('Arguments')) {
        $arguments = $buildScript.Arguments
    }

    $wslScriptPath = Convert-ToWslPath -WindowsPath $scriptPath
    $wslArgs = @('--exec', 'env', $goPathPrefix, '/bin/bash', $wslScriptPath) + $arguments
    Write-Host "Running $($buildScript.Description) build script..."
    & wsl @wslArgs

    if ($LASTEXITCODE -ne 0) {
        throw "Script $scriptPath failed with exit code $LASTEXITCODE"
    }
}

$ciScript = Join-Path $repoRoot 'scripts/win_wsl_setup_ci_env.sh'
if (Test-Path $ciScript) {
    Write-Host 'Running WSL CI environment setup script...'
    $ciScriptWsl = Convert-ToWslPath -WindowsPath $ciScript
    $ciArgs = @('--exec', 'env', $goPathPrefix, '/bin/bash', $ciScriptWsl)
    & wsl @ciArgs
    if ($LASTEXITCODE -ne 0) {
        throw 'win_wsl_setup_ci_env.sh failed with exit code ' + $LASTEXITCODE
    }
} else {
    Write-Warning 'win_wsl_setup_ci_env.sh not found; skipping WSL environment initialization.'
}

Write-Host 'Finished building test apps.'
