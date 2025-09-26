@REM install choco if necessary
if ! command -v choco &> /dev/null
then
    echo "Chocolatey not found, installing..."
    @"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -InputFormat None -ExecutionPolicy Bypass -Command "iex ((New-Object System.Net.WebClient).DownloadString('https://chocolatey.org/install.ps1'))"
    RefreshEnv
fi

# install test app frameworks
choco install -y nodejs go python3

# Build go applications
cd tests;
Get-ChildItem -Directory "go-e2e-*" | ForEach-Object { \
    Write-Host "Building in $($_.Name)"; \
    Push-Location $_.Name; \
    go build -o "21.go_test_app.exe"; \
    go build -o "22.go_test_app.exe"; \
    go build -o "23.go_test_app.exe"; \
    Pop-Location \
}