# Sync the skills with the docs

The weekly docs-sync workflow (`.github/workflows/docs-sync.yaml`) started you in a partial checkout of the
metalbear-co/mirrord repository. It holds only `skills/`, the Markdown pages under `docs/docs/`, and the config
schemas `mirrord-schema.json` and `mirrord-up-schema.json` at the root, so don't look for other files. The
skills are under `skills/skills/`. The workflow gives you the path of a directory, `DOCS_SYNC_DIR`, which holds two
files:

- `docs-changes.diff`: every change to the Markdown pages under `docs/docs/` since the skills were last synced.
- `docs-commits.log`: the commits that made those changes, one per line.

## Your task

Update the skills under `skills/skills/` so that they stay accurate against the current docs. Only change a skill if
the docs changes make its content outdated, incomplete or wrong.

1. Read `docs-changes.diff` and `docs-commits.log` to understand what changed in the docs.
2. Read the frontmatter `description` of each `skills/skills/*/SKILL.md` to find the skills that cover the changed
   topics.
3. For each skill that the changes affect:
   - Read the relevant pages under `docs/docs/`. The diff shows what changed, and the current pages are the source of
     truth for how mirrord works now.
   - Update `SKILL.md`, `README.md` and the files under `references/`, except the four exempt files listed below,
     so that their instructions, flags, config fields and examples match the current docs.
   - Keep the existing structure, heading style and tone. Make the smallest edits that make the skill accurate again,
     and don't rewrite sections that are still correct.

Never edit these four exempt files. They are copies of files generated elsewhere (the configuration reference, the
config schemas and the operator chart's values), and nothing in this repository updates them, so they can be out of
date. Don't use them as the source of truth either: the current config schemas are `mirrord-schema.json` and
`mirrord-up-schema.json` at the repository root.

- `skills/skills/mirrord-config/references/configuration.md`
- `skills/skills/mirrord-config/references/schema.json`
- `skills/skills/mirrord-operator/references/values.yaml`
- `skills/skills/mirrord-up/references/schema.json`

Don't change `skills/.claude-plugin/plugin.json`. The workflow raises the plugin's version itself after you finish,
when you changed a skill.

## Fence tags

Every fenced `json` or `yaml` block that you add or edit needs a tag after the language in the fence info string:

- `mirrord` for a complete `mirrord.json`, or `mirrord=<path>` for a fragment of one, for example
  `json mirrord=feature.db_branches[]`. The path is the dotted path in `mirrord-schema.json` of the object that the
  block's top-level keys belong to, with `[]` for array items and `*` for map values.
- `mirrord-up` for a complete `mirrord-up.yaml`, or `mirrord-up=<path>` for a fragment of one, with the path taken
  from `mirrord-up-schema.json`, for example `yaml mirrord-up=services.*`.
- One of `mirrord-invalid`, `chaos-rule`, `crd`, `helm-values`, `k8s`, `workflow`, `output` or `other` for anything
  else.

A block tagged as config (`mirrord`, `mirrord=`, `mirrord-up`, `mirrord-up=`) must parse as strict JSON or YAML and be
valid against the schema, so it can't hold `...` placeholders, `//` comments or trailing commas. Put explanations in
the prose instead. A pull request check fails on an untagged block or an invalid config block. The full convention is
in `skills/CONTRIBUTING.md`.

## Rules

- Edit only files under `skills/skills/`. The workflow fails the run if any other file changed.
- Don't touch the skills that the docs changes aren't relevant to.
- Don't invent behavior, flags or config fields that the pages under `docs/docs/` don't describe.
- Never include the names of customers or other companies from the docs or the commit messages.
- If no docs change affects a skill, make no edits, and say so in your final message.
