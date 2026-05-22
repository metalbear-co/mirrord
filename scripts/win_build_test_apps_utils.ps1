Set-StrictMode -Version Latest

function Update-Environment {   
    $locations = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment',
                 'HKCU:\Environment'

    $locations | ForEach-Object {   
        $k = Get-Item $_
        $k.GetValueNames() | ForEach-Object {
            $name  = $_
            $value = $k.GetValue($_)
            Set-Item -Path Env:\$name -Value $value
        }
    }
}

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
        & go build -buildvcs=false -o $OutputName | Out-Null
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

function Test-RequiresUnavailableDeps {
    param(
        [string]$ModulePath
    )

    $goModPath = Join-Path $ModulePath 'go.mod'
    if (-not (Test-Path $goModPath)) {
        return $false
    }

    $goModContent = Get-Content $goModPath -ErrorAction SilentlyContinue
    if ($goModContent -match 'v8go') {
        return $true
    }

    return $false
}

function Should-SkipGoModule {
    param(
        [string]$ModulePath
    )

    $goFiles = Get-ChildItem -Path $ModulePath -Recurse -Filter '*.go'
    if (-not $goFiles) {
        return $false
    }

    $allExcluded = $true;
    foreach ($file in $goFiles) {
        if (-not (Select-String -Path $file.FullName -Pattern '^\s*//\s*(go:build|\+build).*?(!windows|linux)' -Quiet)) {
            $allExcluded = $false
            break
        }
    }
    if ($allExcluded) {
        Write-Host "Skipping Go module at $ModulePath because it excludes windows"
        return $true
    }

    if (Test-RequiresUnavailableDeps -ModulePath $ModulePath) {
        Write-Host "Skipping Go module at $ModulePath because it requires unavailable dependencies"
        return $true
    }

    return $false
}

function Build-GoE2EApps {
    param(
        [string]$TestsDir,
        [array]$GoTargets = @(
            @{ Output = '24.go_test_app.exe'; Toolchain = 'go1.24.7' },
            @{ Output = '25.go_test_app.exe'; Toolchain = 'go1.25.1' },
            @{ Output = '26.go_test_app.exe'; Toolchain = 'go1.26.0' }
        )
    )

    Push-Location $TestsDir
    try {
        $goDirs = Get-ChildItem -Directory -Filter 'go-e2e-*'
        if ($goDirs.Count -eq 0) {
            Write-Warning 'No go-e2e-* directories found; skipping Go build steps.'
        }

        foreach ($dir in $goDirs) {
            Write-Host "Building in $($dir.Name)"
            Push-Location $dir.FullName
            try {
                $modulePath = (Get-Location).ProviderPath
                if (Should-SkipGoModule -ModulePath $modulePath) {
                    continue
                }
                foreach ($target in $GoTargets) {
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
}

function Build-RepoGoApps {
    param(
        [string]$RepoRoot,
        [string]$OutputPrefix
    )

    if ([string]::IsNullOrWhiteSpace($OutputPrefix)) {
        throw "Output prefix for Go apps cannot be empty"
    }

    $goFiles = Get-ChildItem -Path $RepoRoot -Recurse -Filter 'go.mod' |
        Where-Object {
            $_.FullName -notmatch '\\node_modules\\' -and
            $_.FullName -notmatch '\\venv\\' -and
            $_.FullName -notmatch '\\target\\'
        }

    foreach ($goMod in $goFiles) {
        Push-Location $goMod.Directory.FullName
        try {
            $modulePath = (Get-Location).ProviderPath
            if (Should-SkipGoModule -ModulePath $modulePath) {
                continue
            }

            Write-Host "Building Go test app in $($goMod.Directory.FullName)"
            & go build -buildvcs=false -o "$OutputPrefix.go_test_app"
            if ($LASTEXITCODE -ne 0) {
                throw "go build failed in $($goMod.Directory.FullName) with exit code $LASTEXITCODE"
            }
        } finally {
            Pop-Location
        }
    }
}

function Build-RustApps {
    param(
        [string]$RepoRoot,
        [string]$Target = $null,
        [string[]]$Crates = @('rust-websockets', 'rust-sqs-printer')
    )

    Push-Location $RepoRoot
    try {
        $args = @('build')
        if ($Target) {
            $args += @('--target', $Target)
        }
        foreach ($crate in $Crates) {
            $args += @('-p', $crate)
        }

        Write-Host "Building Rust test applications: $($Crates -join ', ')"
        & cargo @args
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

# Install the .NET SDK the cs-e2e app needs at test time. It runs as a
# file-based program (`dotnet run Program.cs`), so `dotnet` must be on
# PATH or mirrord's binary resolution fails with
# `BinaryWhichError("dotnet", ...)`. `actions/setup-dotnet` can't be used
# here: the self-hosted runner user has no write access to
# `C:\Program Files\dotnet`. So fetch the official installer and drop the
# SDK into a per-user dir, then prepend it to PATH (and to GITHUB_PATH so
# the later test step sees it too).
function Install-DotnetSdk {
    param(
        [string]$Channel = '10.0',
        [string]$InstallDir = (Join-Path $env:USERPROFILE '.dotnet')
    )

    $dotnetExe = Join-Path $InstallDir 'dotnet.exe'
    if (-not (Test-Path $dotnetExe)) {
        Write-Host "Installing .NET SDK ($Channel) to $InstallDir"
        $installer = Join-Path $env:TEMP 'dotnet-install.ps1'
        Invoke-WebRequest -Uri 'https://dot.net/v1/dotnet-install.ps1' -OutFile $installer -UseBasicParsing
        & $installer -Channel $Channel -InstallDir $InstallDir -NoPath
        if ($LASTEXITCODE -ne 0) {
            throw "dotnet-install.ps1 failed with exit code $LASTEXITCODE"
        }
    } else {
        Write-Host ".NET SDK already present at $InstallDir"
    }

    # Prepend to PATH for this process and persist for subsequent CI steps.
    $env:PATH = "$InstallDir;$env:PATH"
    if ($env:GITHUB_PATH) {
        $InstallDir | Out-File -Append -FilePath $env:GITHUB_PATH -Encoding utf8
    }
    Write-Host "Using dotnet at $dotnetExe"
}

# Build the cs-e2e file-based apps OUTSIDE mirrord, where the build-time NuGet
# restore can reach the real network. The test then runs the produced `.exe`
# under mirrord -- nothing compiles or restores under the layer (which would
# otherwise route the restore through the pod and fail), only the finished
# binary runs. Each app is one file-based `.cs` named after what it tests
# (e.g. AsyncText/AsyncText.cs -> bin/AsyncText.exe); this builds every such
# file under cs-e2e/ into a sibling `bin/`, so adding an app needs no CI change.
function Build-CsE2EApps {
    param([string]$TestsDir)

    $csRoot = Join-Path $TestsDir 'cs-e2e'
    if (-not (Test-Path $csRoot)) {
        Write-Host "No cs-e2e/ directory at $csRoot; skipping C# build."
        return
    }

    $apps = Get-ChildItem -Path $csRoot -Recurse -Filter '*.cs'
    if ($apps.Count -eq 0) {
        Write-Host "No .cs apps found under $csRoot; nothing to build."
        return
    }

    foreach ($app in $apps) {
        $outDir = Join-Path $app.DirectoryName 'bin'
        Write-Host "Building C# e2e app $($app.FullName) -> $outDir"
        & dotnet build $app.FullName --output $outDir --nologo --verbosity minimal
        if ($LASTEXITCODE -ne 0) {
            throw "dotnet build $($app.FullName) failed with exit code $LASTEXITCODE"
        }
    }
}
