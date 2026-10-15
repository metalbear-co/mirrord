# mirrord-operator

Install and operate the mirrord Operator for team and enterprise environments.

## What it does

This skill helps AI agents:
- **Install / upgrade** the mirrord operator via Helm (`metalbear/mirrord-operator`)
- **Authenticate** it — cloud API key (default), license key, or air-gapped PEM / license server
- **Enable features** — queue splitting, DB branching, preview environments, multi-cluster
- **Configure** internal registries, TLS, RBAC, OpenShift, and GKE Autopilot
- **Troubleshoot** operator issues

## Example prompts

```
"Install the mirrord operator on my cluster"

"Set up mirrord for my team with a cloud API key"

"Install the operator in an air-gapped cluster"

"Enable Kafka splitting / Postgres DB branching / preview environments"

"Use our internal registry for the operator images"

"Operator pod is not starting, help me debug"
```

## Prerequisites

- Kubernetes cluster with admin access, and Helm 3.x
- A mirrord for Teams license — register at [app.metalbear.com](https://app.metalbear.com). No license yet? An agent can start a 7-day Enterprise trial for the user — see "No License Yet? Agent-Started Trials" in `SKILL.md` (confirm with the user first — it creates an external organization).

## Quick install

```bash
helm repo add metalbear https://metalbear-co.github.io/charts
helm repo update
curl https://raw.githubusercontent.com/metalbear-co/charts/main/mirrord-operator/values.yaml --output values.yaml

# Create a Secret for the cloud API key (generated in the dashboard → Settings), then reference it:
#   cloud:
#     apiKey:
#       keyRef: mirrord-operator-cloud-api-key
# Write the key to a file (exact contents, no trailing newline) and use --from-file, so the key
# isn't exposed in argv (ps) or shell history; then delete the file.
kubectl create secret generic mirrord-operator-cloud-api-key \
  --namespace mirrord --from-file=apiKey=./apikey.txt
rm apikey.txt

helm install -f values.yaml mirrord-operator metalbear/mirrord-operator
mirrord operator status
```

> **Never** put a cloud API key, license key, or PEM on the command line via `--set` or inline in a committed `values.yaml` — use a Secret ref (`cloud.apiKey.keyRef`, `license.keyRef`, `license.pemRef`) or a Google Secret Manager ref.

## References

- `references/troubleshooting.md` — common operator issues
- `references/helm-values.md` — the important chart values (auth, feature flags, agent, TLS, registry, platform)

## Learn more

- [Operator install docs](https://metalbear.com/mirrord/docs/getting-started/installing-mirrord/operator)
- [Chart values.yaml](https://raw.githubusercontent.com/metalbear-co/charts/main/mirrord-operator/values.yaml)
- [Pricing & plan tiers](https://metalbear.com/mirrord/pricing/)
