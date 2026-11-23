# mirrord docs

## Overview

Paths in this file are relative to this directory of the mirrord repository, which holds the Documentation tab of the [mirrord docs](https://metalbear.com/mirrord/docs), built by GitBook from the Markdown under `docs/`. GitBook builds the site from the `docs-published` branch, which every mirrord release updates to the `docs/` of the released commit, so a merged page reaches the site with the next mirrord release, or sooner if a publish runs between releases (see [Publishing a Fix Between Releases](#publishing-a-fix-between-releases)). `mirrord mcp` embeds the pages, so it gets them only with the next release. Each publish replaces the branch's content, so a page change merged in the GitBook editor is lost at the next publish: make page changes here.

A page for an unreleased change names the versions expected to ship it. Every mirrord release publishes the merged pages and ships them in `mirrord mcp`, so a page for a CLI feature can merge with the feature. The operator ships on its own schedule, so a page for an unreleased operator feature merges only once the operator version that ships it is out. Otherwise a mirrord release would document a feature that users can't get yet.

The Configuration tab is generated from the doc comments in `mirrord/config/src` at the repository root, and the Changelog tab from each product's `CHANGELOG.md`. Fix them there.

## Command Reference

### Linting

```bash
# plan tags
python3 .github/scripts/check_plan_tags.py
```

CI runs it in the `content_check` job of the root `.github/workflows/ci.yaml`. There is no local build, and GitBook doesn't build pull requests to the mirrord repository, so they get no preview.

## Publishing a Fix Between Releases

A fix that can't wait for the next release goes out through the `Publish docs and guides` workflow (`.github/workflows/publish-docs.yaml` at the repository root), which publishes `docs/` to `docs-published` and `guides/` to `guides-published`.

The `publish-docs` pull request label publishes the docs and guides without needing a release, when the pull request merges (`.github/workflows/publish-docs-on-merge.yaml` at the repository root). It publishes everything merged to `docs/` or `guides/` since the last publish, not only that pull request.

The label publishes nothing if it was added after the merge, or if the pull request came from a fork. A maintainer then publishes the change with the steps below.

A failed labeled publish posts to `#ci-alerts`, and re-running the failed job publishes the current `main`.

Run the `Publish docs and guides` workflow by hand instead to see what would go out before it publishes, to leave out pages that must wait, or to publish a change that has already merged:

1. If the fix hasn't merged, merge it to `main` like any other change, without the `publish-docs` label. The next release then keeps the fix, and this merge publishes nothing. Another pull request that merges with the label would still publish it, along with the rest of `main`.
2. From the Actions tab, with "Use workflow from" left on `main`, run the workflow with `ref` set to `main` and `dry_run` on. The step summary names the commit that `main` resolved to and lists the files that differ from the published branches. Everything merged to `docs/` or `guides/` since the last publish goes out with the fix, so check that the list holds only what is ready to publish. If both branches already match, there is nothing to publish.
3. If the list holds pages that must wait, such as one for a CLI feature that merged but isn't released yet, create a branch from the latest release tag, cherry-pick the fix onto it, push it, and dry-run that branch instead. Any name works except one that starts with `docs/`, which git refuses because the repository has a `docs` branch.
4. Run the workflow again with `ref` set to the commit SHA from the dry run's summary and `dry_run` off, so it publishes the commit that the dry run checked. The workflow pushes, waits for GitBook to import the commits, and posts to `#ci-alerts` if any of it fails. Delete the branch from step 3 afterwards.

A release publishes the head of its release branch, so a fix that merged to `main` after the branch was cut disappears from the site when that release publishes, whichever way the fix was published. The fix comes back with the next publish that contains it: the next labeled merge, the next release, or a run of steps 2 to 4.

The workflow waits up to five minutes in all for GitBook to import the commits that it pushed, and fails if GitBook reports a failed import, or no successful one in that time. For a branch that it didn't change, it reads the status once, without waiting: a failed import there fails the run, so a re-run after a failure still fails, while a missing or pending status only warns. A commit that GitBook never imports leaves the site at the previous publish. Check the space's Git Sync in GitBook, and try syncing it again from there. GitBook's Git Sync troubleshooting guide also says that a new commit on the branch makes GitBook import all the content again, and the next publish that changes `docs/` (or `guides/`, for `guides-published`) pushes one.

The workflow refuses a `ref` that is older than the latest release. It also can't push a change to `docs/.github/workflows/`, because GitHub doesn't let the workflow token change workflow files. That publish fails, and its error prints a `git` command that fetches the published branch, builds the publish commit on top of it and pushes it. Run the command in bash or zsh with a token that has the `workflow` scope, then run the workflow again to publish `guides/`. The command ends by printing the parent of the commit that it pushed, so if that commit turns out wrong, force-pushing the parent to the branch restores it.

## Pages

- A page's label in `docs/SUMMARY.md` is its title, and an optional quoted title after the path is its sidebar text. GitBook ignores `title` front matter, but `mirrord mcp` shows it, so keep it equal to the label.
- `description` front matter is the meta description and the subtitle under the title: one sentence on what the page is for.
- Open the body with a `# Title` line matching the label; without it, GitBook renders `##` as `<h3>`. Don't skip heading levels.
- A moved or renamed page gets a redirect in `.gitbook.yaml`. Point it at an existing page, never at another redirect.

## Changes That Reach Several Pages

A change in behavior can make pages false that it does not touch: grep `docs/` for the config field, flag, or Helm value it alters, and fix every page that describes it.

A new database engine or queue service goes into every list of its siblings, including a row in `docs/reference/feature-matrix.md` and the many lists in `docs/sharing-the-cluster/queue-splitting.md`. Some lists name only a few siblings, so grep for several (`grep -rliE 'MariaDB|ClickHouse|Spanner' docs`, `grep -rliE 'RabbitMQ|BullMQ|Temporal' docs`). An engine with a default branch image also gets a row in the default images table in `docs/managing-mirrord/operator.md`.

## Style Guideline

The readers are customers: engineers scanning for an answer. Write terse and factual.

- Change as little as possible. Tighten or correct a sentence before adding one, and leave a sentence alone once it is true.
- State the behavior. No rationale asides (why a version is pinned, how a component works underneath), and no consequence that follows from what the page already says.
- Mirror the closest sibling page: its sections, their order, and its hints.
- Use plain English: many readers speak English as a second language.
- American spelling and Oxford commas.
