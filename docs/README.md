# mirrord docs

This repo contains the source for the [mirrord docs site](https://metalbear.com/mirrord/docs) Documentation tab. It uses [GitBook](https://gitbook.com/) to generate documentation from markdown.

> **NOTE** that the mirrord docs' Configuration tab is not governed by this repo, but by [the mirrord repo docs branch](https://github.com/metalbear-co/mirrord/tree/docs) and is updated automatically by mirrord's release workflow.

> The Changelog tab also has [a separate repo](https://github.com/metalbear-co/docs-changelog), and is also updated automatically by CI.

### Structure

The root of the GitBook docs is `./docs`, as defined in `.gitbook.yaml`. See [the GitBook docs](https://gitbook.com/docs/getting-started/git-sync/content-configuration) for more info.

### Development

**When you open a PR with changes**, GitBook will publish a preview and display the link under the `Checks` section at the bottom of the page that will show your changes - it does so via the [GitBook bot](https://github.com/gitbook-bot) installed in this repo. Use the preview labeled "*GitBook - metalbear.com*".

Changes to **content and basic structure** are possible in the repo, and other changes must be made via the [GitBook platform](https://app.gitbook.com). To do so, you will need access to the MetalBear organisation.

When **changing the structure** of the docs, like creating a new page, you must update `./docs/SUMMARY.md` to reflect this. New folders should have a `README.md` as their overview page.

**Every feature page needs a plan tag.** Pages under `docs/using-mirrord`, `docs/sharing-the-cluster` and `docs/use-cases` must list at least one of `oss`, `team` or `enterprise` in their `tags` frontmatter, alongside `alpha` or `beta` where relevant. `mirrord mcp` reads these tags to tell users which plan a feature needs. The `Plan tags` workflow runs `.github/scripts/check_plan_tags.py` on every PR and fails on any page in those directories without one, so a new page cannot ship untagged.

### GitBook

Each tab on the docs site corresponds to a GitBook "space". Adding a space is done through GitBook, and GitHub sync must be set up to sync changes between the space's repo and the website. Note that changes made in GitBook may be live.
