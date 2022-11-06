# How to Run the E2E tests

To run the tests locally with the latest mirrord-agent image, run the following in the mirrord directory:
- `docker build -t test . --file mirrord/agent/Dockerfile`
- `minikube image load test` (you might have to specify `-p <PROFILE-NAME>` as well)
- `cargo test --package tests --lib -- tests --nocapture`

The name "test" is hardcoded for the CI, and the tests will fail with an `Elapsed` error if the image named "test" is not found. 
To use a different image change the environment variable `MIRRORD_AGENT_IMAGE` at `test_server_init` in `tests/src/utils.rs`.

To use the latest release of mirrord-agent, comment out the line adding the `MIRRORD_AGENT_IMAGE` environment. 
