# `mirrord-tests`

## Overview

`mirrord-tests` is the end-to-end suite: tests that run apps with the mirrord CLI against a Kubernetes cluster.

- Every test always compiles, and the nextest profiles in `.config/nextest.toml` select which tests run.
- A test that should run with both kinds of agent, the job and the ephemeral container, takes
  `#[values(AgentMode::Job, AgentMode::Ephemeral)]` (`utils::AgentMode`).
- Tests run in parallel on one cluster, so name what a test creates with `utils::random_string()`.
- A test that changes the whole cluster, or needs a different kind of cluster, gets its own nextest profile and Bazel
  target, as `operator_install` and `traffic::ipv6` do.
- Each target in `BUILD.bazel` names the cluster that it runs on, and creates the cluster if it doesn't exist
  (`scripts/e2e_runner.py`). It lists the apps that its tests run in its `data`, and only installs Argo Rollouts with
  `argo_rollouts = True`, so a test of a new app or of a rollout needs both in its target.
