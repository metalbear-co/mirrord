## Quick Reference

```bash
cargo check -p mirrord-config --keep-going
cargo test -p mirrord-config

# Proc-macro crate
cargo check -p mirrord-config-derive --keep-going

# Regenerate mirrord-schema.json (run after any config shape/doc change)
cargo test -p mirrord-config check_schema_file_exists_and_is_valid_or_create_it -- --ignored --nocapture
```

## Architecture

### Two-layer model: file config vs runtime config

Most public config structs derive `MirrordConfig`. The derive macro generates a `*FileConfig` type where all fields become `Option<_>`, plus an implementation that resolves final values into the runtime struct.

- `#[config(nested)]` means the field is resolved through `FromMirrordConfig::Generator`.
- `#[config(toggleable)]` wraps nested generators with `ToggleableConfig`, so users can write `true`/`false` as shorthand.

### How values are resolved

Value resolution uses `MirrordConfigSource` combinators (`.or`, `.layer`), chaining env + file values. Precedence: CLI args > env vars > config file.

**Important:** Don't use `std::env` directly during config generation or verification, use `ConfigContext` instead. It supports overrides and strict-env mode for tests.

Resolved configs are passed to downstream processes via `LayerConfig::encode`/`decode` and the `MIRRORD_RESOLVED_CONFIG` env var.

### Validation

`LayerConfig::verify` checks for conflicting settings across features (targetless/copy-target/operator, incoming filter exclusivity, fs buffer limits, startup retry bounds, etc.). Sub-verifiers live in feature modules (`dns`, `outgoing`, `split_queues`).
