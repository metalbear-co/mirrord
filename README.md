# mirrord docs: configuration

This folder contains the source for the mirrord docs site [Configuration tab](https://metalbear.com/mirrord/docs/configuration). It uses [GitBook](https://gitbook.com/) to generate documentation from markdown.

> **NOTE** that the mirrord docs' other tabs are synced to the [docs](https://github.com/metalbear-co/docs) and [docs-changelog](https://github.com/metalbear-co/docs-changelog) repos.

### Structure

The root of the GitBook docs is `./configuration`, as defined in `.gitbook.yaml`. See [the GitBook docs](https://gitbook.com/docs/getting-started/git-sync/content-configuration) for more info.

We used to use [medschool](https://github.com/metalbear-co/mirrord/tree/main/medschool) to generate markdown from the Rustdoc comments in the mirrord source.

### Development

**When you change mirrord's configuration** or its doc comments, medschool will automatically run on the next release to update the `docs` branch, which is synced to GitBook. You should never have to manually update the configuration docs.

**When you open a PR with changes to the contents of this folder**, GitBook will publish a preview and display the link under the `Checks` section at the bottom of the page that will show your changes - it does so via the [GitBook bot](https://github.com/gitbook-bot) installed in this repo. Use the preview labelled "*GitBook - metalbear.com*".

### Non-trivial changes

> IMPORTANT: changes made to the docs in the GitBook UI will be overwritten on the next release! Make changes in code only, or change the relevant CI job.

Changes to **content and basic structure** are possible in the repo, and other changes must be made via the [GitBook platform](https://app.gitbook.com). To do so, you will need access to the MetalBear organisation.

When **changing the structure** of the docs, like creating a new page, you must update `./configuration/SUMMARY.md` to reflect this. New folders should have a `README.md` as their overview page.

### GitBook

Each tab on the docs site corresponds to a GitBook "space". Adding a space is done through GitBook, and GitHub sync must be set up to sync changes between the space's repo and the website. Note that changes made in GitBook may be live.
