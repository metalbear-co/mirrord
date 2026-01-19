param(
    [string]$ServiceAccount = "174145836594-compute@developer.gserviceaccount.com",
    [string]$Project = "mirrord",
    [string]$Zone = "us-central1-a",
    [string]$Instance = "k3s-tests-1",
    [int]$Port = 6443,
    [int]$LocalPort = 6443,
    [int]$Retries = 5
)

$ErrorActionPreference = "Stop"

# 1. Ensure Dependencies
if (-not (Get-Command kubectl -ErrorAction SilentlyContinue)) {
    Write-Host "Installing kubectl..."
    choco install kubernetes-cli -y
}

# 2. Configure gcloud
Write-Host "Configuring gcloud..."
gcloud config set account $ServiceAccount
gcloud config set project $Project
gcloud config set core/disable_prompts true
# Diagnostic info (optional, can be noisy)
# gcloud info
# gcloud auth list

# 3. Check for Existing Tunnel
$ExistingConn = Test-NetConnection -ComputerName "127.0.0.1" -Port $LocalPort -InformationLevel Quiet
if ($ExistingConn) {
    Write-Host "Port $LocalPort is already listening. Checking Kubernetes connectivity..."
    $env:KUBECONFIG = "$env:USERPROFILE\.kube\config"
    kubectl get pods -A
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Tunnel is active and healthy."
        exit 0
    }
    Write-Warning "Port is open but Kubectl failed. Killing stale processes..."
    
    Get-Process -Name "gcloud" -ErrorAction SilentlyContinue | Stop-Process -Force
    $connections = Get-NetTCPConnection -LocalPort $LocalPort -ErrorAction SilentlyContinue
    if ($connections) {
        foreach ($conn in $connections) { Stop-Process -Id $conn.OwningProcess -Force -ErrorAction SilentlyContinue }
    }
}

# 4. Start Tunnel
Write-Host "Starting IAP Tunnel to $Instance ($Zone)..."
$LogFile = "$env:TEMP\iap_tunnel.log"
$ErrFile = "$env:TEMP\iap_tunnel.err"

$GcloudArgs = "compute start-iap-tunnel $Instance $Port --local-host-port=127.0.0.1:$LocalPort --zone=$Zone"
$Process = Start-Process -FilePath "cmd.exe" -ArgumentList "/c gcloud $GcloudArgs" -NoNewWindow -PassThru -RedirectStandardOutput $LogFile -RedirectStandardError $ErrFile

# 5. Wait for Connection
for ($i=1; $i -le $Retries; $i++) {
    Start-Sleep -Seconds 2
    
    if ($Process.HasExited) {
        Write-Error "Tunnel process exited prematurely (Exit Code: $($Process.ExitCode))"
        break
    }

    if (Test-NetConnection -ComputerName "127.0.0.1" -Port $LocalPort -InformationLevel Quiet) {
        Write-Host "Tunnel Established."
        break
    }
    Write-Host "Waiting for tunnel... ($i/$Retries)"
}

# 6. Verify Again
if (-not (Test-NetConnection -ComputerName "127.0.0.1" -Port $LocalPort -InformationLevel Quiet)) {
    Write-Error "Failed to establish IAP tunnel on port $LocalPort."
    Write-Host "--- STDOUT ---"; if (Test-Path $LogFile) { Get-Content $LogFile }; Write-Host "--------------"
    Write-Host "--- STDERR ---"; if (Test-Path $ErrFile) { Get-Content $ErrFile }; Write-Host "--------------"
    
    if (-not $Process.HasExited) { Stop-Process -Id $Process.Id -Force }
    exit 1
}

# 7. Final K8s Check
$env:KUBECONFIG = "$env:USERPROFILE\.kube\config"
if (-not (Test-Path $env:KUBECONFIG)) {
    Write-Error "KUBECONFIG not found at $env:KUBECONFIG"
    exit 1
}
kubectl get pods -A
if ($LASTEXITCODE -ne 0) {
    Write-Error "Tunnel open but kubectl failed."
    exit 1
}

Write-Host "IAP Tunnel Ready."
