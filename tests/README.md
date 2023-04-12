# How to Run the E2E tests

To run the tests locally with the latest mirrord-agent image, run the following in the mirrord directory:
- `docker build -t test . --file mirrord/agent/Dockerfile`
- `minikube image load test` (you might have to specify `-p <PROFILE-NAME>` as well)
- `cargo test --package tests`

The name `test` is hardcoded for the CI, and the tests will fail with an `Elapsed` error if the image named `test` is not found. 
To use a different image change the environment variable `MIRRORD_AGENT_IMAGE` at `test_server_init` in `tests/src/utils.rs`.

To use the latest release of mirrord-agent, comment out the line adding the `MIRRORD_AGENT_IMAGE` environment. 

# Cleanup

The Kubernetes resources created by an E2E test are automatically deleted when the test exists successfully. However, failed tests by default don't delete their resources to allow debugging. You can force resource deletion by setting a `MIRRORD_E2E_FORCE_CLEANUP` variable to any value.

```bash
MIRRORD_E2E_FORCE_CLEANUP=y cargo test --package tests
```

All test resources share a common label `MIRRORD_E2E_TEST_RESOURCE=true`. To delete them, simply run:

```bash
kubectl delete namespaces,deployments,services -l MIRRORD_E2E_TEST_RESOURCE=true
```
