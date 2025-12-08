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

# Check if WSL has any distributions installed
Write-Host 'Checking for WSL distributions...'
$wslInstalled = $false
try {
    $wslListOutput = & wsl --list --quiet 2>&1
    if ($LASTEXITCODE -eq 0) {
        # Parse the output to see if any distributions are installed
        # Filter out header lines and empty lines
        $distributions = $wslListOutput | Where-Object { 
            $_ -and 
            $_ -notmatch '^\s*$' -and 
            $_ -notmatch '^\s*NAME' -and 
            $_ -notmatch '^\s*--' -and
            $_ -notmatch '^\s*Windows Subsystem for Linux'
        }
        if ($distributions) {
            $wslInstalled = $true
            Write-Host "Found WSL distribution(s): $($distributions -join ', ')"
        }
    }
} catch {
    Write-Warning "Error checking WSL distributions: $($_.Exception.Message)"
}

if (-not $wslInstalled) {
    Write-Host 'No WSL distribution found. Checking WSL prerequisites...'
    
    # Check if Virtual Machine Platform is enabled (required for WSL2)
    $vmPlatformEnabled = $false
    try {
        $vmPlatform = Get-WindowsOptionalFeature -Online -FeatureName "VirtualMachinePlatform" -ErrorAction SilentlyContinue
        if ($vmPlatform -and $vmPlatform.State -eq "Enabled") {
            $vmPlatformEnabled = $true
            Write-Host "Virtual Machine Platform is enabled."
        } else {
            Write-Host "Virtual Machine Platform is not enabled."
        }
    } catch {
        Write-Warning "Could not check Virtual Machine Platform status: $($_.Exception.Message)"
    }
    
    # Check if WSL feature is enabled
    $wslFeatureEnabled = $false
    try {
        $wslFeature = Get-WindowsOptionalFeature -Online -FeatureName "Microsoft-Windows-Subsystem-Linux" -ErrorAction SilentlyContinue
        if ($wslFeature -and $wslFeature.State -eq "Enabled") {
            $wslFeatureEnabled = $true
            Write-Host "WSL feature is enabled."
        } else {
            Write-Host "WSL feature is not enabled."
        }
    } catch {
        Write-Warning "Could not check WSL feature status: $($_.Exception.Message)"
    }
    
    # Try to enable required features if not enabled
    if (-not $vmPlatformEnabled -or -not $wslFeatureEnabled) {
        Write-Host 'Attempting to enable required Windows features for WSL...'
        Write-Host 'This requires administrator privileges and may require a reboot.'
        
        try {
            if (-not $wslFeatureEnabled) {
                Write-Host 'Enabling WSL feature...'
                Enable-WindowsOptionalFeature -Online -FeatureName "Microsoft-Windows-Subsystem-Linux" -NoRestart -ErrorAction Stop
                Write-Host 'WSL feature enabled.'
            }
            
            if (-not $vmPlatformEnabled) {
                Write-Host 'Enabling Virtual Machine Platform feature...'
                Enable-WindowsOptionalFeature -Online -FeatureName "VirtualMachinePlatform" -NoRestart -ErrorAction Stop
                Write-Host 'Virtual Machine Platform feature enabled.'
            }
            
            Write-Warning "Windows features have been enabled. A reboot may be required."
            Write-Warning "After reboot, run this script again to install the WSL distribution."
        } catch {
            Write-Error "Failed to enable Windows features. Error: $($_.Exception.Message)"
            Write-Error "You may need to run this script as Administrator."
            Write-Error "Or enable features manually:"
            Write-Error "  Run as Administrator:"
            Write-Error "    Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Windows-Subsystem-Linux"
            Write-Error "    Enable-WindowsOptionalFeature -Online -FeatureName VirtualMachinePlatform"
            Write-Error "  Then reboot and run this script again."
            throw "Required Windows features are not enabled. Enable them and try again."
        }
    }
    
    Write-Host 'Attempting to install Ubuntu...'
    try {
        # Try to install Ubuntu (default distribution)
        # Note: This may require admin rights and might prompt for reboot
        $installOutput = & wsl --install -d Ubuntu 2>&1
        $installExitCode = $LASTEXITCODE
        $installOutputString = $installOutput | Out-String
        Write-Host $installOutputString
        
        # Check for specific error messages
        if ($installOutputString -match 'HCS_E_HYPERV_NOT_INSTALLED' -or 
            $installOutputString -match 'Virtual Machine Platform' -or
            $installOutputString -match 'virtualization is enabled in the BIOS') {
            Write-Error "WSL2 requires Virtual Machine Platform to be enabled."
            Write-Error "The feature may have been enabled but a reboot is required."
            Write-Error "Please reboot your system and run this script again."
            Write-Error "If the issue persists after reboot, ensure:"
            Write-Error "  1. Virtualization is enabled in BIOS"
            Write-Error "  2. Hyper-V or Virtual Machine Platform is enabled in Windows Features"
            throw "WSL2 installation failed: Virtual Machine Platform not available. Reboot may be required."
        }
        
        if ($installExitCode -eq 0) {
            Write-Host 'Ubuntu installation command completed successfully.'
            # Wait a moment and check again
            Start-Sleep -Seconds 3
            $wslListOutput = & wsl --list --quiet 2>&1
            $distributions = $wslListOutput | Where-Object { 
                $_ -and 
                $_ -notmatch '^\s*$' -and 
                $_ -notmatch '^\s*NAME' -and 
                $_ -notmatch '^\s*--' -and
                $_ -notmatch '^\s*Windows Subsystem for Linux'
            }
            if (-not $distributions) {
                Write-Warning "WSL distribution installation may require a reboot or additional setup."
                Write-Warning "Please check if Ubuntu is being installed, and if prompted, reboot your system."
                Write-Warning "After reboot, run this script again to continue."
                throw "WSL distribution not yet available. Installation may be in progress or a reboot may be required."
            } else {
                Write-Host "WSL distribution is now available: $($distributions -join ', ')"
            }
        } else {
            Write-Warning "WSL install command returned exit code: $installExitCode"
            Write-Warning "This may indicate that installation requires admin rights or a reboot."
            throw "Failed to install WSL distribution. You may need to run as administrator or install manually."
        }
    } catch {
        Write-Error "WSL distribution is required but not installed."
        Write-Error "Please install a WSL distribution manually using one of these methods:"
        Write-Error "  1. Run as Administrator: wsl --install -d Ubuntu"
        Write-Error "  2. Or: wsl --install (to install the default distribution)"
        Write-Error "  3. Or use: wsl --list --online to see available distributions"
        Write-Error "  After installation, you may need to reboot your system."
        throw "No WSL distribution available. Install one and try again. Error: $($_.Exception.Message)"
    }
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
    Write-Host "Running Go provisioning script in WSL..."
    $wslOutput = & wsl --exec /bin/bash $tempGoScriptWsl 2>&1
    $wslExitCode = $LASTEXITCODE
    if ($wslOutput) {
        Write-Host $wslOutput
    }
    if ($wslExitCode -ne 0) {
        Write-Error "WSL command failed with exit code: $wslExitCode"
        Write-Error "This may indicate that:"
        Write-Error "  1. No WSL distribution is installed or available"
        Write-Error "  2. The WSL distribution is not properly initialized"
        Write-Error "  3. There was an error running the Go provisioning script"
        throw "Failed to provision Go inside WSL (exit code: $wslExitCode)"
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
