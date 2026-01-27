param(
    [int]$Retries = 5
)

$ErrorActionPreference = "Stop"

# 1. Ensure Dependencies
if (-not (Get-Command kubectl -ErrorAction SilentlyContinue)) {
    Write-Host "Installing kubectl..."
    choco install kubernetes-cli -y
}

# 2. Check Kubernetes connectivity
Write-Host "Checking Kubernetes connectivity..."
$env:KUBECONFIG = "$env:USERPROFILE\.kube\config"

if (-not (Test-Path $env:KUBECONFIG)) {
    Write-Error "KUBECONFIG not found at $env:KUBECONFIG"
    exit 1
}

Write-Host "--- DEBUG INFO ---"
Write-Host "User: $(whoami)"
Write-Host "KUBECONFIG path: $env:KUBECONFIG"
Write-Host "Config Content:"
Get-Content $env:KUBECONFIG
Write-Host "Active Config View:"
kubectl config view
Write-Host "------------------"

kubectl get pods -A
if ($LASTEXITCODE -ne 0) {
    Write-Error "Kubectl check failed. Cluster is not reachable."
    exit 1
}

Write-Host "Cluster connection verified."
