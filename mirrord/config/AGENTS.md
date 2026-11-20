# `mirrord-config`

## Overview

`mirrord-config` holds the config types and their validation, which the CLI and the layer use to decide what features
are enabled and what target to use.

## Plans

Every option records the plan it needs (`oss`, `team`, or `enterprise`, per its docs page) as `x-mirrord-plan` in the
schema, as a `mirrord_config::plan::Plan`: `#[config(plan = Team)]` on `MirrordConfig` fields,
`#[schemars(extend("x-mirrord-plan" = crate::plan::Plan::Team))]` elsewhere. Nested options inherit it, except that
every option under `feature` and every target kind needs its own; tests in `mirrord-mcp` fail on an option without
one.

## Schemas and Docs

The doc comments here are user documentation. They go into `mirrord-schema.json`, and on each release that changes
it, the release workflow publishes them as the configuration reference, onto this repository's `docs` branch.
`<!--${internal}-->` keeps a doc comment out of the reference. `typos` checks this crate's Rust in CI.

The build generates the schema as `//mirrord/config:schema`, which runs `src/bin/print_schema.rs`, and `mirrord-schema.json`
must be a copy of it. After any change to the types or their docs, copy it again, or
`//mirrord/config:write_schema_test` fails. The same change can reach `mirrord-up-schema.json`, which
`mirrord/up/AGENTS.md` covers.

```bash
bazel run //mirrord/config:write_schema
```

`schema_file_text` sorts the keys of every object, because Bazel builds `serde_json` with `preserve_order`, which would
otherwise write each type's properties in the order of its fields.
