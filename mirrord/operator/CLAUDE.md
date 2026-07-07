## Overview

`mirrord-operator` is the crate that both the CLI and operator depend on for shared types:

- `feature = "crd"`: the Rust types that define CRD specs and statuses, including `schemars` schema generation.
- `feature = "client"`: the operator API client used by the CLI to check operator status, create/connect sessions, and
create some CRDs.

## Command Reference

```bash
cargo check -p mirrord-operator --all-features
```

## Modifying a CRD

When modifying a CRD definition run the `generate_helm_crd_yaml` test in the operator repo to update the generated helm
charts.

```bash
cargo test -p operator-crd generate_helm_crd_yaml -- --ignored --nocapture
```

## Backwards Compatibility

CRD compatibility is the top priority in this crate. CRD schemas are generated from the Rust type definitions via
`schemars` and installed into the Kubernetes cluster. During upgrades, existing CR objects created under older schemas
remain in the cluster. If the new operator can't deserialize them, reconciliation loops fail permanently.

## Hard Rules

- Prefer additive changes. New fields should usually be `Option<T>` + `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  For containers like `Vec<T>` or `BTreeMap<K, V>`, don't wrap in `Option`, just use `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
  (or `BTreeMap::is_empty`), which is enough for backward compatibility.

- Prefer stable/deterministic containers (`BTreeMap` over `HashMap`, `BTreeSet` over `HashSet`) in CRD types.

- Don't remove, rename, or change the type of existing serialized fields without a compatibility plan.

- Use forward-compatible enums (`#[serde(other)]` and explicit `Unknown` variants).

- Never use `mirrord-config` types directly in CRD specs/statuses. CRs have much stronger backward compatibility
  requirements than `mirrord-config`, so embedding a config type into a CRD effectively freezes it. Any future change to
  that config type becomes a CRD-breaking change.
