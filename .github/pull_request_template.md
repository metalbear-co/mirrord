Thank you for contributing to mirrord!

Please make sure you add a changelog file in `changelog.d/` named `<identifier>.<category>.md`

**Examples:**
- `1054.changed.md` (with GitHub issue)
- `+some-name.added.md` (without issue)

**Identifier:**
- If a GitHub issue exists, use the issue number from the public [mirrord repo](https://github.com/metalbear-co/mirrord)
- Use `+some-name` if no issue exists
- Don't use Linear issues or private repo issue numbers

**Category:**
Check `towncrier.toml` for available categories (`added`, `changed`, `fixed`, etc.) and choose the one that fits your change.