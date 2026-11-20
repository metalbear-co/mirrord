# `mirrord-up`

## Overview

`mirrord-up` implements `mirrord up`, which runs several mirrord sessions from one `mirrord-up.yaml`.

Every `mirrord-up.yaml` option records the plan it needs as `mirrord-config`'s options do (`mirrord/config/AGENTS.md`),
with `#[schemars(extend("x-mirrord-plan" = mirrord_config::plan::Plan::Team))]`; `every_up_option_has_a_plan` in
`mirrord-mcp` fails on an option without one.

`mirrord-up-schema.json` embeds this crate's config types and `mirrord-config`'s. It's a copy of `//mirrord/up:schema`,
which the build generates with `src/bin/print_schema.rs`. After any change to either crate's types, or to their docs, copy it
again, or `//mirrord/up:write_schema_test` fails:

```bash
bazel run //mirrord/up:write_schema
```
