---
title: License Server Setup
description: Set up the mirrord dashboard served by a self-hosted license server
tags:
  - alpha
  - enterprise
---

# License Server Setup

{% hint style="info" %}
This page covers setup for the dashboard served by a self-hosted license server. See [Dashboard](../admin-dashboard.md) for what the interface looks like once it's running, or [Cloud Setup](cloud.md) if your operator authenticates with a cloud API key instead.
{% endhint %}

This is the dashboard for self-hosted installs, nothing leaves your cluster. It reads from your license server's existing session database, so historical usage data appears immediately once enabled.

## Quick Start

1. Add `dashboard.enabled: true` to your license server Helm values:

```yaml
# values.yaml
dashboard:
  enabled: true
```
2. Upgrade the license server:
```bash 
helm repo update metalbear
helm upgrade mirrord-operator-license-server metalbear/mirrord-license-server -f ./values.yaml --wait
```
3. Via `kubectl port-forward`: Forward the dashboard port to your local machine:
```bash
kubectl port-forward -n mirrord svc/mirrord-operator-license-server 8050:8050
```
4. Open <http://localhost:8050/> in your browser.

Target workload breakdowns (namespace, deployment name) are available for sessions recorded after the upgrade.

{% hint style="info" %}
The dashboard does not require authentication beyond network access to the license server. Access control is handled by your cluster networking configuration.
{% endhint %}

## Helm Configuration

| Setting             | Default | Description                     |
| ------------------- | ------- | ------------------------------- |
| `dashboard.enabled` | `false` | Enable the dashboard            |
| `dashboard.port`    | `8050`  | Port the dashboard is served on |

The chart automatically configures the container port, service port, and required environment variables when `dashboard.enabled` is set to `true`.

## API Endpoints

The dashboard consumes two API endpoints from the license server. These are also available for programmatic access:

* `GET /api/v1/reports/usage?format=json` returns the full usage report (users, targets, sessions, CI metrics, machines).
* `GET /api/v1/reports/usage/trends?days=30` returns time-series data (daily sessions, active users, CI sessions, user adoption).

Both endpoints require the `x-license-key` header. This is the license key configured in the license server's Helm values (`license.key`). When the dashboard is served by the license server, this header is injected automatically. For direct API access (e.g., via `curl`), pass it manually:

```bash
curl -H "x-license-key: <your-license-key>" \
  http://localhost:8050/api/v1/reports/usage?format=json
```

For spreadsheet reports (Excel format), see Getting a Utilisation Report (../license-server.md#getting-a-utilisation-report-from-the-license-server)