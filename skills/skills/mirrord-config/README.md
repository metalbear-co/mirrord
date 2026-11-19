# mirrord-config

Generate, validate, and fix mirrord configuration files.

## What it does

This skill helps AI agents:
- **Generate** valid `mirrord.json` configs from natural language
- **Validate** existing configs against the official schema
- **Fix** invalid configurations with explanations
- **Explain** configuration options and patterns

## Example prompts

```
"Generate a mirrord config for pod api-server in staging namespace"

"Validate my mirrord.json" (paste your config)

"Configure mirrord to steal traffic on port 8080"

"Help me set up HTTP header filtering for my mirrord config"
```

## How it works

1. Looks up options with the mirrord MCP server's `explain_config_option` when connected, otherwise in the configuration reference
2. Generates or validates configs based on your request
3. Validates with the mirrord MCP server's `validate_config` when connected, otherwise with `mirrord verify-config`
4. Returns validated JSON with explanations

## References

This skill uses local reference files:
- `references/schema.json`: mirrord JSON Schema, synced from upstream. The fence tag check validates config examples against it; read it only as a last resort, when neither `validate_config` (mirrord MCP server) nor `mirrord verify-config` is available
- `references/configuration.md` — Configuration reference
