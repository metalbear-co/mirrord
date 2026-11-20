# `mirrord-tests`

## Overview

`mirrord-tests` is the end-to-end suite: tests that run apps with the mirrord CLI against a Kubernetes cluster.

- Every test always compiles, and the nextest profiles in `.config/nextest.toml` select which tests run.
- A test that should run with both kinds of agent, the job and the ephemeral container, takes
  `#[values(AgentMode::Job, AgentMode::Ephemeral)]` (`utils::AgentMode`).
- Tests run in parallel on one cluster. Name what a test creates with `utils::random_string()`. A test that changes
  the cluster as a whole, or needs a cluster of its own kind, gets its own nextest profile, as `operator_install` and
  `traffic::ipv6` do.
