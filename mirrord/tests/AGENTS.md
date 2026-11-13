# `mirrord-tests`

## Overview

`mirrord-tests` is the end-to-end suite.

- Every test is always compiled, and nextest's profiles in `.config/nextest.toml` pick which ones run. A test that
  should run with both kinds of agent, the job and the ephemeral container, takes
  `#[values(AgentMode::Job, AgentMode::Ephemeral)]` (`utils::AgentMode`).
- Tests run in parallel on one cluster. Name what a test creates with `utils::random_string()`. A test that changes
  the cluster as a whole, or needs a cluster of its own kind, gets its own nextest profile, as `operator_install` and
  `traffic::ipv6` do.
