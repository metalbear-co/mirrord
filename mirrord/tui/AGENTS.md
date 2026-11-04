# Agent instructions

## Keep SPEC.md in sync with the code

`SPEC.md` describes the behavior that exists in this crate today. It is the source of truth for "what does the mirrord TUI currently do."

**Whenever you generate, modify, or remove code in this crate, you should update `SPEC.md` in the same change so that it continues to accurately describe the current functionality.**

This applies to any code change which alters user-viewable behaviour, including:

- The screens and tabs, and what each one shows.
- Key bindings.
- The layout, colors, status bar, and connection error dialog.
- How it starts and logs, as `mirrord tui` or as the standalone binary.
- How it connects to the cluster.

### How to update

1. Make the code change.
2. Open `SPEC.md` and edit the affected sections so they describe the new state of the app — not the diff, and not aspirational behavior.
3. Do not add speculative or planned behavior to `SPEC.md`. It documents what the code does right now.

### When a code change does *not* require a SPEC.md update

- Pure refactors that do not change observable behavior.
- Formatting, comment, or rename changes with no behavioral effect.
- Changes to files the spec does not describe (e.g. CI config, this file).

If you are unsure whether a change is observable, err on the side of updating `SPEC.md`.
