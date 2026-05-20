# Compile the AsyncText.exe e2e test app.
#
# Runs `dotnet publish` against the sibling csproj, producing a
# self-contained single-file Windows executable at .\bin\AsyncText.exe.
# The mirrord-tests `Application::AsyncTextCsharp` variant points at
# the same relative path, so no extra wiring is needed.
$ErrorActionPreference = 'Stop'

$dir = $PSScriptRoot
$proj = Join-Path $dir 'AsyncText.csproj'
$out  = Join-Path $dir 'bin\AsyncText.exe'

Write-Host "Building AsyncText.exe from $dir"

# /nologo here -> --nologo on dotnet, and /v:m -> --verbosity minimal so
# the CI log isn't drowned in restore noise.
& dotnet publish $proj --configuration Release --nologo --verbosity minimal `
  | Out-Null

if ($LASTEXITCODE -ne 0) {
    throw "dotnet publish failed with exit code $LASTEXITCODE"
}

if (-not (Test-Path $out)) {
    throw "publish reported success but $out is missing"
}

Write-Host "Built $out"
