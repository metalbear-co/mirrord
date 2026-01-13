# Helper script to start IAP tunnel to k3s-tests-1 on port 6443
# Usage: . ./scripts/start_iap_tunnel.ps1
# Returns $tunnelProc (Process object)

$ErrorActionPreference = "Stop"

# Resolve gcloud.cmd to avoid issues with .ps1 and cmd.exe
$gcloudPath = (Get-Command gcloud).Source
if ($gcloudPath.EndsWith(".ps1")) {
    $parent = Split-Path $gcloudPath -Parent
    $gcloudCmd = Join-Path $parent "gcloud.cmd"
} else {
    $gcloudCmd = $gcloudPath
}
Write-Host "Using gcloud at: $gcloudCmd"

# Clean up any existing process on port 6443 to avoid "Unable to open socket" errors
Write-Host "Checking for existing tunnel on port 6443..."
try {
    $existing = Get-NetTCPConnection -LocalPort 6443 -ErrorAction Stop
    if ($existing) {
        Write-Host "Found existing process on 6443 (PID: $($existing.OwningProcess)). Killing..."
        Stop-Process -Id $existing.OwningProcess -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
} catch {
    Write-Host "No existing process found on port 6443."
}

# Start IAP Tunnel in background
# Use cmd.exe /c to execute the batch file.
Write-Host "Starting IAP tunnel on port 6443..."
$tunnelProc = Start-Process -FilePath "cmd.exe" `
    -ArgumentList "/c ""$gcloudCmd"" compute start-iap-tunnel k3s-tests-1 6443 --local-host-port=127.0.0.1:6443 --zone=us-central1-a --quiet" `
    -NoNewWindow -PassThru

# Wait for tunnel to come up
$retries = 30
$tunnelUp = $false
while ($retries -gt 0) {
    try {
        $conn = New-Object System.Net.Sockets.TcpClient("127.0.0.1", 6443)
        $conn.Close()
        $tunnelUp = $true
        Write-Host "Tunnel is up!"
        break
    } catch {
        Write-Host "Waiting for tunnel... ($retries)"
        Start-Sleep -Seconds 1
        $retries--
    }
}

if (-not $tunnelUp) {
    if ($tunnelProc.HasExited) {
        Write-Error "Tunnel process exited unexpectedly. Exit code: $($tunnelProc.ExitCode)"
    }
    Stop-Process -InputObject $tunnelProc -ErrorAction SilentlyContinue
    throw "Tunnel failed to start on port 6443"
}

# Return the process object for caller to manage if needed
return $tunnelProc
