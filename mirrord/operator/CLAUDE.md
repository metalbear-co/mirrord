## Quick Reference

```bash
# CRD model only
cargo check -p mirrord-operator --features crd --keep-going

# Full client + CRD model used by CLI
cargo check -p mirrord-operator --features client --keep-going
cargo test -p mirrord-operator --features client
```

## What This Crate Does

`mirrord-operator` is the crate that both the CLI and operator depend on for shared types:
- `feature = "crd"`: the Rust types that define CRD specs and statuses, including `schemars` schema generation.
- `feature = "client"`: the operator API client used by the CLI to check operator status, create/connect sessions, and create some CRDs.

## Generating CRD YAML
To generate the actual YAML definition for a CRD, run the `write_all_crd_yamls` test in `src/crd.rs`, which calls `write_crd_yaml` for each CRD, with `MIRRORD_TEST_DUMP_CRD_DIR` pointing to the output directory.
```bash
tmp_dir="$(mktemp -d)"
MIRRORD_TEST_DUMP_CRD_DIR="$tmp_dir" cargo test -p mirrord-operator --features crd write_all_crd_yamls
```
This writes one file per CRD, the one you want is at `$tmp_dir/{crd.metadata.name}`.

## Where CRDs Are Reconciled (in the operator repo)

When changing a CRD here, check its reconciler there:

- `MirrordClusterSession` → `operator/operator/session/src/**`
- `PreviewSession` → `operator/operator/preview-env/src/**`
- `CopyTargetCrd` → `operator/operator/context` + `operator/operator/controller/src/copy_target/**`
- `MirrordWorkloadQueueRegistry` / `MirrordSqsSession` → `operator/operator/sqs-splitting/src/**`
- `MirrordKafka*` CRDs → `operator/operator/kafka-splitting/src/**`
- `*BranchDatabase` CRDs → `operator/operator/db-branching/src/**`
- `MirrordClusterWorkloadPatch*` → `operator/operator/workload-patch/src/**`
- `MirrordOperatorCrd`, `TargetCrd`, `SessionCrd` route handling → `operator/operator/controller/src/{status,target,restful,openapi}.rs`

## Backwards Compatibility

CRD compatibility is the highest-risk area in this crate. CRD schemas are generated from these Rust types via `schemars`, installed into Kubernetes, and validated by the API server. During upgrades, existing CR objects created under older schemas remain in the cluster. If the new operator can't deserialize them, reconciliation loops fail permanently.

### Hard Rules

- Prefer additive changes. New fields should usually be `Option<T>` + `#[serde(default, skip_serializing_if = "Option::is_none")]`. For containers like `Vec<T>` or `BTreeMap<K, V>`, don't wrap in `Option`, just use `#[serde(default, skip_serializing_if = "Vec::is_empty")]` (or `BTreeMap::is_empty`), which is enough for backward compatibility.
- Prefer stable/deterministic containers (`BTreeMap` over `HashMap`, `BTreeSet` over `HashSet`) in CRD types.
- Don't remove, rename, or change the type of existing serialized fields without a compatibility plan.
- Keep `group/version/kind/plural` stable unless you implement a full multi-version migration path.
- Keep `TargetCrd::urlfied_name` and connect URL formats stable (see `client.rs` URL compatibility tests).
- Use forward-compatible enums (`#[serde(other)]` and explicit unknown variants), see `NewOperatorFeature::Unknown`, `KubeTarget` unknown target handling.
- When the wire shape must change, use compatibility wrappers like `CopyTargetEntryCompat` and `LockedPortCompat` (support reading both old and new formats while exposing the new schema).
- Keep old compatibility fields until both old clients and old operators are no longer supported.
- Query param JSON encoding is shared between client (`src/client/connect_params.rs`) and operator parser (`operator/operator/controller/src/restful/params.rs`), keep them aligned.
- Never use `mirrord-config` types directly in CRD specs/statuses. CRs have much stronger backward compatibility requirements than `mirrord-config`, so embedding a config type into a CRD effectively freezes it. Any future change to that config type becomes a CRD-breaking change.
- When adding a new CRD type, add an `#[ignored]` test in `src/crd.rs` that prints its generated schema. This makes it easy to inspect, verify and place the schema in the Helm charts in the operator repo.
