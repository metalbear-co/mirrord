# How to Run the E2E tests

To run the tests locally with the latest mirrord-agent image, run the following in the mirrord directory:

- `docker build -t test . --file mirrord/agent/Dockerfile`
- `minikube image load test` (you might have to specify `-p <PROFILE-NAME>` as well)
- `cargo xtask test-e2e` (builds mirrord from source if no artifacts are provided)

## Without installing the toolchains

Building the test apps and the CLI needs four Go SDKs, Node, Python, protoc and zig. To skip
installing any of them, run inside the prebuilt `e2e-runner` image, which ships every test app of
this repo and of the operator's suite already compiled, and stages them into your checkout on start.
Only the cluster and the agent image stay outside, so do those two steps above first. The named
volumes keep compilation incremental across runs, and keep the container's root-owned build output
out of your checkout:

```bash
docker run --rm -it --network host \
  -v "$PWD:/workspace/mirrord" \
  -v e2e-runner-cargo-registry:/usr/local/cargo/registry \
  -v e2e-runner-cargo-git:/usr/local/cargo/git \
  -v e2e-runner-go:/go \
  -v e2e-runner-target:/workspace/mirrord/target \
  -v "$HOME/.kube:/root/.kube:ro" \
  -w /workspace/mirrord \
  ghcr.io/metalbear-co/e2e-runner:latest \
  bash -c 'cargo xtask build-cli && cargo xtask test-e2e'
```

The staged apps are the ones the image was built from. After editing one, rebuild it with
`scripts/prepare_e2e.sh --apps-only`. The image is defined by `tests/e2e.Dockerfile` and republished
from `main` whenever the test apps, or the toolchains that build them, change.

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
