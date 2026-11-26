# mirrord docs

This directory of the mirrord repository contains the source for the [mirrord docs site](https://metalbear.com/mirrord/docs) Documentation tab. It uses [GitBook](https://gitbook.com/) to generate documentation from markdown.

> **NOTE** that the mirrord docs' Configuration tab is not built from this directory, but from [the mirrord repository's `docs` branch](https://github.com/metalbear-co/mirrord/tree/docs), which mirrord's release workflow updates.

> The Changelog tab also has [a separate repo](https://github.com/metalbear-co/docs-changelog), and is also updated automatically by CI.

### Structure

The root of the GitBook docs is `./docs`, as defined in `.gitbook.yaml`. See [the GitBook docs](https://gitbook.com/docs/getting-started/git-sync/content-configuration) for more info.

### Development

GitBook syncs this directory on the mirrord repository's `main` branch, so a merged change reaches the site once GitBook imports it, while `mirrord mcp` gets it with the next mirrord release. A pull request into `main` from a branch of the mirrord repository gets a preview, linked from its `GitBook (./docs) - metalbear.com/mirrord/docs/` status, and a pull request from a fork gets none. `AGENTS.md` says what to do when GitBook fails to import a change.

Changes to **content and basic structure** can be made in this directory. Other changes, such as a space's settings, must be made via the [GitBook platform](https://app.gitbook.com), because GitBook doesn't store them in this directory. To do so, you will need access to the MetalBear organisation.

When **changing the structure** of the docs, like creating a new page, you must update `./docs/SUMMARY.md` to reflect this. New folders should have a `README.md` as their overview page.

**Every feature page needs a plan tag.** Pages under `docs/using-mirrord`, `docs/sharing-the-cluster` and `docs/use-cases` must list at least one of `oss`, `team` or `enterprise` in their `tags` frontmatter, alongside `alpha` or `beta` where relevant. `mirrord mcp` reads these tags to tell users which plan a feature needs. The `content_check` job of the mirrord repository's CI runs `.github/scripts/check_plan_tags.py` on every PR that changes the docs and fails on any page in those directories without one, so a new page cannot ship untagged.

### GitBook

Each tab on the docs site corresponds to a GitBook "space". Adding a space is done through GitBook, and GitHub sync must be set up to sync changes between the space's repo and the website. Note that changes made in GitBook may be live. GitBook can't push to the mirrord repository's `main`, which only takes pull requests, so a change request merged in the GitBook editor doesn't reach `main` directly. The export fails, or GitBook opens a pull request for it, and the next import from `main` replaces the edit on the site. Make page changes here.
