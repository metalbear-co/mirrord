# How to Run the E2E tests

The full local setup (toolchains, test apps, cluster, agent image, CLI build) is described in
[CONTRIBUTING.md](../CONTRIBUTING.md#getting-started). In short, from the mirrord directory:

- Build the agent image and load it into the cluster (`minikube image load test` or `kind load docker-image test`),
  see [Prepare a cluster](../CONTRIBUTING.md#prepare-a-cluster).
- Build the test apps: `scripts/prepare_e2e.sh --apps-only`.
- Build the CLI with `cargo xtask build-cli` (never plain `cargo build`, on macOS that binary cannot run anything).
- Run `cargo xtask test-e2e --binary <cli> --layer <layer> -- --no-fail-fast`.

## Without installing the toolchains

Run inside the prebuilt `ci-runner` image and you only have to set up the cluster and the agent
image, as in the first step above. Nothing else needs installing.

```bash
cargo xtask in-runner -- bash -c 'cargo xtask build-cli && cargo xtask test-e2e'
```

Build output goes to named volumes, so it survives between runs and the container's root-owned files
stay out of your checkout.

The staged apps are the ones the image was built from. After editing one, rebuild it with
`scripts/prepare_e2e.sh --apps-only`. The image is defined by `tests/e2e.Dockerfile`.

The name `test` is hardcoded for the CI, and the tests will fail with an `Elapsed` error if the image named `test` is
not found.
To use a different image change the environment variable `MIRRORD_AGENT_IMAGE` at `test_server_init` in
`tests/src/utils.rs`.

To use the latest release of mirrord-agent, comment out the line adding the `MIRRORD_AGENT_IMAGE` environment.

# Cleanup

The Kubernetes resources created by the E2E tests are automatically deleted when the test exits. However, you can
preserve resources from failed tests for debugging. To do this, set the `MIRRORD_E2E_PRESERVE_FAILED` variable to any
value.

```bash
MIRRORD_E2E_PRESERVE_FAILED=y cargo xtask test-e2e
```

All test resources share a common label `mirrord-e2e-test-resource=true`. To delete them, simply run:

```bash
kubectl delete namespaces,deployments,services -l mirrord-e2e-test-resource=true
```
