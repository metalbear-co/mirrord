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

1. Reads the official mirrord JSON schema and configuration reference
2. Generates or validates configs based on your request
3. Runs `mirrord verify-config` for authoritative validation
4. Returns validated JSON with explanations

## References

This skill uses local reference files:
- `references/schema.json` — mirrord JSON Schema
- `references/configuration.md` — Configuration reference
