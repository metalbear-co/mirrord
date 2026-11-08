# `mirrord-up`

## Overview

`mirrord-up` implements `mirrord up`, which runs several mirrord sessions from one `mirrord-up.yaml`.

Every `mirrord-up.yaml` option records the plan it needs as `mirrord-config`'s options do (`mirrord/config/AGENTS.md`),
with `#[schemars(extend("x-mirrord-plan" = mirrord_config::plan::Plan::Team))]`; `every_up_option_has_a_plan` in
`mirrord-mcp` fails on an option without one.

`mirrord-up-schema.json` embeds this crate's config types and `mirrord-config`'s. After any change to either, or to
their docs, regenerate it, or `up_schema_file_is_up_to_date` fails:

```bash
cargo test -p mirrord-up generate_up_schema -- --ignored
```
