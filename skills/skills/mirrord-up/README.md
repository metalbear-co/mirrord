# mirrord-up

Run **multiple concurrent mirrord sessions** from one `mirrord-up.yaml` — compose-style lifecycle for related microservices.

## What it does

This skill helps AI agents:

- **Generate** or explain `mirrord-up.yaml` (`common`, `services`, targets, filters, `run`)
- **Guide** `mirrord up`, `mirrord up init`, and CLI flags (`-f`, `--key`, `-m`/`--mode`, `-u`)
- **Explain** service modes: default **split** (auto HTTP filter based on the session key) vs. **replace** (local process takes over the service; the deployed workload is scaled to zero for the session and restored after) vs. **mirror** (matching traffic is copied to the local process while the deployed service keeps serving it, requires mirrord 3.258.0+) — and warn about `replace`'s impact on shared clusters
- **Explain** queue splitting, which works automatically for services in `split` and `mirror` mode (no `mirrord-up.yaml` field needed) for supported brokers — `replace` mode doesn't use it, since the deployed workload is scaled down and the local process consumes its queues directly
- **Explain** Tera templating in `mirrord-up.yaml` (`{{ key }}`, `get_env(...)`) for injecting the session key or per-developer environment values

## Example prompts

```
"How do I run multiple mirrord sessions at once?"

"Generate a mirrord-up.yaml for my auth and payment services"

"What does mirrord up init do?"

"How does the session key work with mirrord up HTTP filters?"

"What's the difference between split and replace mode?"

"How do I use env vars / templating in mirrord-up.yaml?"
```

## Prerequisites

- mirrord CLI installed and on `PATH`
- Cluster access (`kubectl` / kubeconfig) for targeted services
- Optional: operator when using `common.operator: true`

## Quick start

```bash
mirrord up init
mirrord up
```

```yaml mirrord-up
services:
  user-auth-service:
    run:
      command: ["python", "-m", "http.server"]

  stage-user-dashboard-app:
    target:
      path: pod/nginx
    run:
      command: ["node", "app.js"]
```

## Key commands

```bash
mirrord up init
mirrord up init -o ./mirrord-up.yaml
mirrord up
mirrord up -f mirrord-up-custom.yaml
mirrord up --key my-session-key
mirrord up -u   # also start mirrord ui in the background
mirrord up -m replace   # override every service's default_mode for this run
```

## Learn more

- [Multiple concurrent sessions (mirrord up)](https://metalbear.com/mirrord/docs/using-mirrord/multiple-concurrent-sessions)
