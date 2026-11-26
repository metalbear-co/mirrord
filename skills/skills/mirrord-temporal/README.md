# mirrord-temporal

Configure mirrord Operator's Temporal task queue splitting: route only the workflow and activity tasks that match your filter to your local worker, while the deployed worker keeps everything else. (Alpha, Team / Enterprise feature.)

## What it does

This skill helps AI agents:
- **Generate** `MirrordPropertyList` YAML for the Temporal frontend connection (plaintext, TLS, mutual TLS, Temporal Cloud API key)
- **Generate** `MirrordSplitConfig` YAML linking a worker Deployment/StatefulSet/Rollout to its task queues (`kind: temporal`)
- **Write** the developer-facing mirrord.json `feature.split_queues` section with `message_filter` (workflow ID, workflow/activity type, headers, search attributes) and `jq_filter` (task content)
- **Guide** Helm setup: `operator.temporalSplitting`, the proxy port, drain timeout, `max_buffered_tasks`
- **Validate** generated YAML for required fields and cross-references
- **Troubleshoot** with `mirrord queues status` and operator logs

## How Temporal splitting works

Temporal has no native queue splitting, so the operator runs a small gRPC proxy: it polls the real task queue itself, patches the deployed worker onto a virtual task queue behind the proxy, and routes each buffered task to the session whose filter matches it — everything else flows to the deployed worker.

## Example prompts

```
"Set up Temporal task queue splitting for my worker deployment"

"Connect the mirrord operator to Temporal Cloud with an API key"

"Route only workflows whose ID starts with test-local- to my laptop"

"Filter Temporal activity tasks by their input payload with jq"

"Why are my Temporal tasks piling up during a mirrord session?"
```

## Prerequisites

- mirrord Operator **3.170.0+** with `operator.temporalSplitting: true`, mirrord CLI **3.221.0+**
- Temporal splitting is an **alpha** feature (Team / Enterprise)
- The worker must read its task queue name from an environment variable the operator can see

## Learn more

- [Temporal queue splitting docs](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting/temporal)
- [Queue Splitting overview](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting)
