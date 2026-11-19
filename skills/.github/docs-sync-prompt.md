# Sync skills with docs changes

You are running inside the metalbear-co/skills repository. A checkout of the
metalbear-co/docs repository (the mirrord documentation) is available at
`docs-src/`. The file `/tmp/docs-changes.diff` contains everything that changed
in the docs since the last time the skills were synced, and
`/tmp/docs-commits.log` lists the corresponding commit messages.

## Your task

Update the skills under `skills/` so they stay accurate against the current
docs. Only touch a skill if the docs diff makes its content outdated,
incomplete, or wrong.

Steps:

1. Read `/tmp/docs-changes.diff` and `/tmp/docs-commits.log` to understand what
   changed in the docs.
2. Read the frontmatter `description` of each skill under `skills/*/SKILL.md`
   to map docs topics to skills.
3. For each skill affected by the diff:
   - Read the relevant current docs pages under `docs-src/docs/` for full
     context. The diff shows what changed; the docs pages are the source of
     truth for how things work now.
   - Update `SKILL.md`, `README.md`, and files under `references/` so their
     instructions, flags, config fields, and examples match the current docs.
     These files are overwritten verbatim by
     `.github/workflows/update-references.yml`, so never edit them:
     - `skills/mirrord-config/references/configuration.md`
     - `skills/mirrord-config/references/schema.json`
     - `skills/mirrord-operator/references/values.yaml`
     - `skills/mirrord-up/references/schema.json`
   - Keep the existing file structure, headings style, and tone. Make the
     smallest edits that restore accuracy — do not rewrite sections that are
     still correct.
   - Bump the `version` field in the skill's frontmatter by one minor version
     (e.g. `"1.1"` → `"1.2"`), once per edited skill.

## Fence tags

Every fenced `json` or `yaml` block you add or edit needs a tag after the
language in the fence info string: `mirrord` for a complete `mirrord.json`,
`mirrord=<schema.path>` for a fragment (for example
`json mirrord=feature.db_branches[]`, with the path taken from
`skills/mirrord-config/references/schema.json`), `mirrord-up` or
`mirrord-up=<path>` for `mirrord-up.yaml`, or one of `mirrord-invalid`,
`chaos-rule`, `crd`, `helm-values`, `k8s`, `workflow`, `output`, `other` for
everything else. `mirrord-up=<path>` paths are taken from
`skills/mirrord-up/references/schema.json`. Blocks tagged as config must parse as strict JSON or YAML, so
no `...` placeholders or `//` comments: put explanations in the prose instead.
A PR check rejects untagged blocks. The full convention is in `CONTRIBUTING.md`.

## Rules

- Never edit anything under `docs-src/`, `.github/`, `assets/`, the repo-root
  `README.md`, `LICENSE`, or `install.sh`.
- Do not touch skills the docs diff is not relevant to.
- Do not invent behavior, flags, or config fields that you cannot find in the
  docs checkout.
- Never include customer or company names from the docs or commit messages.
- If none of the docs changes affect any skill, make no edits at all and state
  that clearly in your final message.
