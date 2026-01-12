<#
.SYNOPSIS
    Connects to k3s-tests-1 via GCloud IAP Tunnel (Port 6443) and launches k9s.
    Identical logic to user's k3s-connect.sh.
#>

$ErrorActionPreference = "Stop"

$InstanceName = "k3s-tests-1"
$Zone = "us-central1-a"
$LocalPort = 6443
$RemotePort = 6443

# 1. CHECK KUBECONFIG
$KubePath = "$env:USERPROFILE\.kube\config"
if ($env:KUBECONFIG) {
    $KubePath = $env:KUBECONFIG
}

if (-not (Test-Path $KubePath)) {
    Write-Error "Kubeconfig not found at $KubePath. Please ensure the configuration file exists."
    Write-Host "For CI: This file is populated from the K3S_TESTS_KUBECONFIG secret."
    Write-Host "For Manual: Place the verified localhost-compatible config at $KubePath."
    exit 1
}

$env:KUBECONFIG = $KubePath
Write-Host "Using KUBECONFIG: $KubePath"

# 2. CLEANUP
Write-Host "Stopping existing IAP listeners..."
Get-Process -Name "gcloud" -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -like "*start-iap-tunnel*" } | Stop-Process -Force

# Also kill any local port listeners
$connections = Get-NetTCPConnection -LocalPort $LocalPort -ErrorAction SilentlyContinue
if ($connections) {
    foreach ($conn in $connections) {
        Stop-Process -Id $conn.OwningProcess -Force -ErrorAction SilentlyContinue
    }
}

# 2. START IAP TUNNEL
Write-Host "Starting IAP Tunnel ($LocalPort -> $RemotePort)..."
# gcloud compute start-iap-tunnel k3s-tests-1 6443 --local-host-port=127.0.0.1:6443 --zone=us-central1-a --quiet
$Args = @("compute", "start-iap-tunnel", $InstanceName, "$RemotePort", "--local-host-port=127.0.0.1:$LocalPort", "--zone=$Zone", "--quiet", "--no-user-output-enabled")

$IapJob = Start-Job -ScriptBlock {
    param($Args)
    Start-Process -FilePath "gcloud" -ArgumentList $Args -NoNewWindow -Wait
} -ArgumentList (,$Args)

Start-Sleep -Seconds 5
if ($IapJob.State -eq 'Running') {
    Write-Host "IAP Tunnel Started (Job Id: $($IapJob.Id))"
} else {
    Write-Error "IAP Tunnel Failed to Start."
    Receive-Job $IapJob
    exit 1
}

# 3. VERIFY TUNNEL KUBECONFIG
Write-Host "Verifying Kubeconfig..."
# (Config is already set in env:KUBECONFIG)

# 4. LAUNCH K9S
Write-Host "Launching k9s..."
if (-not (Get-Command k9s -ErrorAction SilentlyContinue)) {
    choco install k9s -y
}

# Wait a bit more for IAP to stabilize
Start-Sleep -Seconds 2

if (Test-NetConnection -ComputerName "127.0.0.1" -Port 6443 -InformationLevel Quiet) {
    k9s
} else {
    Write-Error "Tunnel port 6443 is not open yet."
    kubectl get pods -A
}
