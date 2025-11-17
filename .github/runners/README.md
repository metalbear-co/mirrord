# Self-Hosted GitHub Actions Runners on GCP

This directory contains scripts and configuration for setting up self-hosted GitHub Actions runners on Google Cloud Platform (GCP) for the mirrord project.

## Overview

Self-hosted runners provide:
- **Faster builds**: More CPU cores, persistent caching
- **Cost efficiency**: Lower cost at scale compared to GitHub-hosted runners
- **Customization**: Full control over environment and dependencies

## Architecture

- **Linux Docker Runner**: `n1-standard-8` (8 vCPU, 30GB RAM) - For Docker builds
- **Linux Tests Runner**: `n1-standard-4` (4 vCPU, 15GB RAM) - For integration tests
- **Persistent Disks**: 200GB SSD for Rust and Docker caching

## Prerequisites

1. **GCP Account**: Access to GCP project `mirrord`
2. **gcloud CLI**: Installed and authenticated
   ```bash
   gcloud auth login
   gcloud config set project mirrord
   ```
3. **GitHub Token**: Personal Access Token with `repo` and `admin:org` scopes
   - Create at: https://github.com/settings/tokens/new?scopes=repo,admin:org

## Setup Steps

### 1. Create GCP Infrastructure

Run the GCP setup script to create VMs, networks, and disks:

```bash
cd .github/runners
chmod +x gcp-setup.sh
./gcp-setup.sh
```

This provisions, by default:
- `linux-docker-1` and `linux-docker-2` (n1-standard-8) for Docker-heavy jobs
- `linux-tests-1` and `linux-tests-2` (n1-standard-4) for integration tests

Need a different mix? You can still override the runner list:

```bash
# Create just one extra docker builder
./gcp-setup.sh --create-runner linux-docker-3 n1-standard-8

# Create a custom pool
./gcp-setup.sh \
  --create-runner linux-docker-3 n1-standard-8 \
  --create-runner linux-tests-3 n1-standard-4
```

This creates:
- VPC network and subnet
- Firewall rules
- VM instances for each runner type
- Persistent disks for caching

### 2. Install Runners on VMs

For each VM, SSH in and install the runner:

```bash
# For Linux Docker runner
gcloud compute ssh github-runner-linux-docker --zone=us-central1-a
```

Then on the VM:

```bash
# Download the installation script
curl -O https://raw.githubusercontent.com/metalbear-co/mirrord/main/.github/runners/install-runner.sh
chmod +x install-runner.sh

# Run with your GitHub token
GITHUB_TOKEN=ghp_xxx \
RUNNER_NAME=linux-docker \
RUNNER_LABELS=self-hosted,linux,x64,docker \
./install-runner.sh
```

Repeat for the Linux Tests runner:

```bash
gcloud compute ssh github-runner-linux-tests --zone=us-central1-a
```

```bash
GITHUB_TOKEN=ghp_xxx \
RUNNER_NAME=linux-tests \
RUNNER_LABELS=self-hosted,linux,x64 \
./install-runner.sh
```

### 3. Verify Runners

Check that runners are online in GitHub:
- Go to: https://github.com/metalbear-co/mirrord/settings/actions/runners

### 4. Update CI Workflow

The CI workflow (`.github/workflows/ci.yaml`) has been updated to use self-hosted runners where appropriate. The workflow will automatically use self-hosted runners when available.

## Runner Labels

- `self-hosted`: All self-hosted runners
- `linux`: Linux runners
- `x64`: x86_64 architecture
- `docker`: Runners with Docker (for Docker builds)

## Caching

Runners use persistent disks mounted at `/mnt/cache` for:
- **Rust cache**: `CARGO_TARGET_DIR=/mnt/cache/rust/target`
- **Docker cache**: Persistent Docker layer cache
- **Cargo registry**: `CARGO_HOME=/mnt/cache/cargo-registry`

## Maintenance

### Check Runner Status

```bash
# SSH into VM
gcloud compute ssh github-runner-<name> --zone=us-central1-a

# Check service status
systemctl status actions.runner.metalbear-co.mirrord.<name>.service

# View logs
journalctl -u actions.runner.metalbear-co.mirrord.<name>.service -f
```

### Update Runner

```bash
# SSH into VM
cd /home/runner/actions-runner
sudo -u runner ./svc.sh stop
sudo -u runner ./svc.sh uninstall

# Download latest runner
./install-runner.sh  # Re-run installation script
```

### Restart Runner

```bash
systemctl restart actions.runner.metalbear-co.mirrord.<name>.service
```

## Cost Estimation

Based on GCP pricing (as of 2024):
- **n1-standard-8**: ~$0.38/hour (~$273/month if running 24/7)
- **n1-standard-4**: ~$0.19/hour (~$137/month if running 24/7)
- **Persistent SSD (200GB)**: ~$40/month per disk
- **Total**: ~$490/month for 2 runners (24/7)

**Note**: Runners can be stopped when not in use to save costs. Consider using preemptible VMs for additional savings.

## Security Considerations

1. **Network Isolation**: Runners are in a dedicated VPC subnet
2. **Firewall Rules**: Only egress traffic allowed
3. **Service Account**: Uses minimal GCP service account permissions
4. **Runner Tokens**: Automatically rotated by GitHub Actions

## Troubleshooting

### Runner Not Appearing in GitHub

1. Check runner service is running:
   ```bash
   systemctl status actions.runner.*.service
   ```

2. Check logs for errors:
   ```bash
   journalctl -u actions.runner.*.service -n 100
   ```

3. Verify network connectivity:
   ```bash
   curl -I https://github.com
   ```

### Build Failures

1. Check disk space:
   ```bash
   df -h /mnt/cache
   ```

2. Clear cache if needed:
   ```bash
   rm -rf /mnt/cache/rust/target/*
   ```

3. Check Docker:
   ```bash
   docker info
   docker system df
   ```

## Cleanup

To remove all resources:

```bash
# Delete VMs
gcloud compute instances delete github-runner-linux-docker github-runner-linux-tests --zone=us-central1-a

# Delete disks
gcloud compute disks delete runner-linux-docker-cache runner-linux-tests-cache --zone=us-central1-a

# Delete network (if not used elsewhere)
gcloud compute networks delete github-runners-network
```

