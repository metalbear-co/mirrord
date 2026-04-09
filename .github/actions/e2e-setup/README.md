# e2e-setup

Vendored from `metalbear-co/ci/e2e-setup-action`.

Used by mirrord CI to prepare the shared E2E cluster environment:

1. Download and load the prebuilt `mirrord-tests-image` artifact.
2. Start minikube with the requested container runtime.
