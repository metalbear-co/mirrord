# mirrord-prev-env

Create and manage mirrord **preview environments** — run a modified service as an isolated pod in a shared Kubernetes cluster, scoped by an environment key and traffic filter, to validate and review changes against real traffic without affecting live services.

## What it does

This skill helps AI agents:
- **Run** preview environments ad hoc with `mirrord preview start` / `status` / `stop`
- **Wire** preview environments into CI with the `metalbear-co/mirrord-preview` GitHub Action (e.g. per-PR previews), or by calling the CLI directly, including least-privilege cluster access via the `mirrord-operator-ci` ClusterRole
- **Share** a preview via a plain HTTPS link with `mirrord-share-ingress`, optionally with a stable (pre-existing) host
- **Target** a CronJob, not just a Deployment/StatefulSet/Rollout
- **Build** the `mirrord.json` that drives a preview (target, traffic filter, TTL, timeout)
- **Explain** traffic isolation via the environment `key` + `header_filter`, and header propagation across HTTP / gRPC / Kafka / SQS
- **Troubleshoot** preview-specific issues (licensing, registry/image pulls, `--force` replacement, traffic routing, the never-Ready readinessGate, teardown)

## Two modes

1. **Ad-hoc (developer)** — `mirrord preview start -f mirrord.json -i <image> -k <key>`, `mirrord preview status`, `mirrord preview logs --key <key>` (see why a preview failed), `mirrord preview stop --key <key>`.
2. **CI (GitHub Action)** — `metalbear-co/mirrord-preview` starts/stops previews across a PR lifecycle (or drive the CLI directly in CI).

## Example prompts

```
"How do I run a mirrord preview environment?"

"Set up per-PR preview environments with mirrord in GitHub Actions"

"What config and flags does `mirrord preview start` need?"

"How do I route only my traffic to the preview pod?"

"Check and stop a preview session"
```

## Key commands

```bash
# Ad-hoc
mirrord preview start -f mirrord.json -i myrepo/myapp:tag -k pr-123
mirrord preview status
mirrord preview logs --key pr-123   # what the preview's pods wrote (requires mirrord 3.255.0+, operator+chart 3.205.0+)
mirrord preview stop --key pr-123
```

```yaml workflow
# CI
- uses: metalbear-co/mirrord-preview@main
  with:
    action: start          # or: stop
    target: deployment/my-app
    image: myrepo/myapp:${{ github.sha }}
    filter: 'baggage: mirrord-session={{ key }}'
    key: pr-${{ github.event.pull_request.number }}
```

## Prerequisites

- mirrord **Operator 3.142.0+** deployed with `operator.previewEnv: true` (default on recent chart versions; set it explicitly on older ones)
- mirrord **CLI 3.189.0+** (the Action installs the latest automatically)
- **Enterprise** plan (a free trial is minted as Enterprise; an agent with no license can start one itself — see the `mirrord-operator` skill)
- Reachable kubeconfig (laptop or CI runner)
- A **built and pushed** container image for the preview pod — pushed to the **same registry/repository the target already pulls from** (the preview pulls with the target's credentials)

## Learn more

- [mirrord Preview Environments docs](https://metalbear.com/mirrord/docs/use-cases/preview-environments)
- [Preview Environments in CI docs](https://metalbear.com/mirrord/docs/use-cases/preview-environments-in-ci)
- [mirrord-preview GitHub Action](https://github.com/metalbear-co/mirrord-preview)
- [Reference workflow (playground)](https://github.com/metalbear-co/playground/blob/main/.github/workflows/preview-shop-pr.yml)
