# `mirrord-tests`

## Overview

`mirrord-tests` is the end-to-end suite.

- Gate a new test on what it targets with `#[cfg_attr(not(feature = "..."), ignore)]`, using the features in
  `Cargo.toml`. No workflow here or in the operator repository runs the tests behind `operator`; that repository's
  CI only compiles them.
- Tests run in parallel on one cluster. Name what a test creates with `utils::random_string()`. A test that changes
  the cluster as a whole gets its own nextest profile, as `operator_install` does (`.config/nextest.toml`).
