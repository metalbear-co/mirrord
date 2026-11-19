# Contributing

## Fence tags for json and yaml blocks

Every fenced `json` or `yaml` block in a Markdown file carries one tag after the language in the fence info string, for example ` ```json mirrord `. The tag says whether the block is mirrord config and, for fragments, where it sits in the schema. Fences in other languages (`bash`, `toml`, `text`, ...) are not covered.

`.github/scripts/check_fence_tags.py` runs on every pull request and fails on an untagged block, a tag outside the set below, or a config block (`mirrord`, `mirrord=`, `mirrord-up`, `mirrord-up=`) that does not parse as strict JSON or YAML or contains a `...` placeholder. Fragment paths and the fragment's root keys are resolved against the synced schemas, so a path that does not exist or a key that is not a property at that path also fails. Config blocks are then validated against the same schemas (JSON Schema Draft 7): a complete block as a whole, a fragment as the subtree at its path. A fragment may leave out required sibling keys, but every value it has must validate. A failure names the file, the line and the path inside the block where validation failed. Run it locally with `python3 .github/scripts/check_fence_tags.py` after `python3 -m pip install -r .github/scripts/requirements.txt` (PyYAML, markdown-it-py and jsonschema).

The schemas default to the synced copies listed under [Fragment paths](#fragment-paths). `--mirrord-schema <file>` and `--mirrord-up-schema <file>` replace them, so the mirrord repo can run this same script against the schemas a build generates.

### Tag set

| Tag | Use for |
| --- | --- |
| `mirrord` | A complete `mirrord.json` (or its YAML form with `yaml mirrord`) |
| `mirrord=<path>` | A fragment of `mirrord.json` |
| `mirrord-up` | A complete `mirrord-up.yaml` |
| `mirrord-up=<path>` | A fragment of `mirrord-up.yaml` |
| `mirrord-invalid` | Config that is wrong on purpose, for example in troubleshooting docs |
| `chaos-rule` | A `mirrord chaos` rule, or part of one |
| `crd` | A mirrord custom resource (`MirrordPropertyList`, `MirrordSplitConfig`, ...), or part of one |
| `helm-values` | Helm values for the operator chart |
| `k8s` | Any other Kubernetes manifest (RBAC, ServiceAccount, ...) |
| `workflow` | CI pipeline config (GitHub Actions, GitLab CI, CircleCI) |
| `output` | Command output |
| `other` | Anything else. Prefer a specific tag when one fits |

Config blocks must parse: no `...` placeholders, no `//` comments, no trailing commas. Put explanations in the prose above the block. Template strings such as `"{{ key }}"` are fine inside quoted values.

### Fragment paths

`<path>` is the dotted schema path of the object the block's top-level keys belong to. Use `[]` for array items and `*` for map values. `mirrord=<path>` resolves against [`skills/mirrord-config/references/schema.json`](skills/mirrord-config/references/schema.json). `mirrord-up=<path>` resolves against [`skills/mirrord-up/references/schema.json`](skills/mirrord-up/references/schema.json).

A block whose root keys are `copy` and `connection` sits inside one entry of `feature.db_branches`:

```json mirrord=feature.db_branches[]
{ "connection": { "url": "DATABASE_URL" }, "copy": { "mode": "schema" } }
```

A block whose root key is `run` sits inside one service of `mirrord-up.yaml`:

```yaml mirrord-up=services.*
run:
  command: ["node", "app.js"]
```

### Examples

| Info string | Example body |
| --- | --- |
| `json mirrord` | `{ "target": "deployment/api", "feature": { "network": { "incoming": "steal" } } }` |
| `yaml mirrord` | `target: deployment/api` |
| `json mirrord=feature.network.incoming` | `{ "mode": "steal" }` |
| `yaml mirrord-up` | `services: { api: { run: { command: ["node", "app.js"] } } }` |
| `yaml mirrord-up=services.*` | `target: { path: deployment/api }` |
| `json mirrord-invalid` | `{ "feature": { "network": { "incoming": "stael" } } }` |
| `json chaos-rule` | `{ "name": "slow db", "priority": 10, "selector": { "upstream": "db" }, "effect": { "latency": { "read_ms": 750 } } }` |
| `yaml crd` | `kind: MirrordPropertyList` |
| `yaml helm-values` | `operator: { kafkaSplitting: true }` |
| `yaml k8s` | `kind: ClusterRoleBinding` |
| `yaml workflow` | `on: [push, pull_request]` |
| `json output` | `{ "type": "Success", "warnings": [] }` |
| `json other` | `{ "globalPassThroughEnv": ["MIRRORD_*"] }` (a `turbo.json`) |

### Exempt files

These files are overwritten verbatim by `.github/workflows/update-references.yml`, so they are not tagged and the check skips them. Keep this list in sync with `EXEMPT` in `.github/scripts/check_fence_tags.py`.

- `skills/mirrord-config/references/configuration.md`
- `skills/mirrord-config/references/schema.json`
- `skills/mirrord-operator/references/values.yaml`
- `skills/mirrord-up/references/schema.json`
