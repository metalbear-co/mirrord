param(
    [int]$Retries = 5
)

$ErrorActionPreference = "Stop"

# 1. Ensure Dependencies
if (-not (Get-Command kubectl -ErrorAction SilentlyContinue)) {
    Write-Host "Installing kubectl..."
    choco install kubernetes-cli -y
}

# 2. Setup Kubeconfig (Fetch from Secret Manager)
Write-Host "Setting up Kubeconfig..."
$env:KUBECONFIG = "$env:USERPROFILE\.kube\config"
$SecretName = "k3s-tests-kubeconfig"
$Project = "mirrord"

try {
    # Ensure parent dir exists
    $ParentDir = Split-Path -Parent $env:KUBECONFIG
    if (-not (Test-Path $ParentDir)) { New-Item -ItemType Directory -Force -Path $ParentDir | Out-Null }
    
    Write-Host "Fetching secret '$SecretName' from project '$Project'..."
    $SecretContent = gcloud secrets versions access latest --secret=$SecretName --project=$Project
    
    if (-not $SecretContent) { throw "Empty secret content." }
    
    $SecretContent | Set-Content -Path $env:KUBECONFIG -Encoding UTF8
    Write-Host "Successfully retrieved and updated kubeconfig from Secrets Manager."
}
catch {
    Write-Warning "Failed to fetch kubeconfig from secrets manager: $_"
    Write-Warning "Falling back to existing local file (if present)..."
}

# 3. Check Kubernetes connectivity
if (-not (Test-Path $env:KUBECONFIG)) {
    Write-Error "KUBECONFIG not found at $env:KUBECONFIG and Secret Manager fetch failed."
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
