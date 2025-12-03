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
            @{ Output = '23.go_test_app.exe'; Toolchain = 'go1.23.12' },
            @{ Output = '24.go_test_app.exe'; Toolchain = 'go1.24.7' },
            @{ Output = '25.go_test_app.exe'; Toolchain = 'go1.25.1' }
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
            & go build -o "$OutputPrefix.go_test_app"
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
        [string]$Target = 'x86_64-pc-windows-msvc',
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
