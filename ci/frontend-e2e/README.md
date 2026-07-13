# Frontend e2e harness

Cluster-side fixtures for the cross-repo frontend test harness (session monitor,
config wizard, browser extension). Runs on the `han-dev` cluster in the
`mirrord-test` GCP project, never on the shared playground.

Every run provisions its own state under a unique run ID and asserts protocol
outcomes, never pre-existing sessions or a developer's current kube context.

## Pieces

- `provision.sh <run-id> [context]` creates the `ci-fe-<run-id>` namespace
  (labelled `mirrord-ci=frontend-e2e`) with an echo workload and quota, and
  prints the target coordinates for `mirrord exec`.
- `teardown.sh <run-id> [context]` deletes the namespace.
- `echo.yaml` is the fixture workload: an echo server that reflects request
  headers, so tests can assert both traffic directions (the staging service
  receives the mirrord header; the local process receives the mirrored copy).
- `reaper.yaml` is a cluster-side CronJob (namespace `ci-frontend`) that
  deletes any `mirrord-ci=frontend-e2e` namespace older than two hours.
  CI teardown is best-effort; the reaper is the guarantee. Apply once per
  cluster: `kubectl apply -f reaper.yaml`.

## Probe rules

Service networking and DNS in a fresh namespace can lag pod readiness by a few
seconds. Every request a test makes must carry a timeout (`curl -m`) and retry;
a first-shot request without one can hang forever on a blackholed SYN.
