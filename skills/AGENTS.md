# Agent instructions

Paths in this file are relative to this directory of the mirrord repository.

## Fence tags

Every fenced `json` or `yaml` block you add or edit in a Markdown file needs a tag after the language in the fence info string: `mirrord` for a complete `mirrord.json`, `mirrord=<schema.path>` for a fragment (for example `json mirrord=feature.db_branches[]`), `mirrord-up` or `mirrord-up=<path>` for `mirrord-up.yaml`, or one of `mirrord-invalid`, `chaos-rule`, `crd`, `helm-values`, `k8s`, `workflow`, `output`, `other` for everything else. Config blocks must parse as strict JSON or YAML, so no `...` placeholders or `//` comments. Fragment paths and root keys are resolved against the schemas at the repository root, `mirrord-schema.json` and `mirrord-up-schema.json`, so the path must exist and every root key must be a property there. Config blocks are also validated against those schemas, so they must be valid config, not just parse. Run `python3 .github/scripts/check_fence_tags.py --mirrord-schema ../mirrord-schema.json --mirrord-up-schema ../mirrord-up-schema.json` from this directory before finishing (install its dependencies, PyYAML, markdown-it-py and jsonschema, first: `python3 -m pip install -r .github/scripts/requirements.txt`), as the `content_check` job of the root `.github/workflows/ci.yaml` does. Without the flags, it uses the copies under `skills/*/references/`, which nothing updates in the mirrord repository. The full tag set, path rule and exempt files are in [CONTRIBUTING.md](CONTRIBUTING.md).

## Plugin version

A change to `skills/` or `.claude-plugin/`, the files that Claude Code installs, must raise `version` in `.claude-plugin/plugin.json` above the version on `main`, because Claude Code updates an installed plugin only when its version changes. The `plan` job of the mirrord repository's CI checks this. [CONTRIBUTING.md](CONTRIBUTING.md#plugin-version) says which other manifests to leave alone and what to do when the check fails in the merge queue.
