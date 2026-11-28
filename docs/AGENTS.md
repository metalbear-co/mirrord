# mirrord docs

## Overview

Paths in this file are relative to this directory of the mirrord repository, which holds the Documentation tab of the [mirrord docs](https://metalbear.com/mirrord/docs), built by GitBook from the Markdown under `docs/`. GitBook's Documentation space syncs this directory on `main` (Git Sync with the project directory `docs`), so a merged page reaches the site once GitBook imports its merge commit (see [Publishing](#publishing)). `mirrord mcp` embeds the pages, so it gets them only with the next release. GitBook can't push to `main`, which only takes pull requests, so a change request merged in the GitBook editor doesn't reach `main` directly. The export fails, or GitBook opens a pull request for it, and the next import from `main` replaces the edit on the site. Make page changes here.

A page goes live when it merges, which can be before the release that ships its change, so a page for an unreleased change must name the versions expected to ship it. Pages for operator features follow the same rule: merge them with the feature. The operator ships on its own schedule, so the site, and the next `mirrord mcp`, can show such a page before that operator version is out.

The Configuration tab is generated from the doc comments in `mirrord/config/src` at the repository root, and the Changelog tab from each product's `CHANGELOG.md`. Fix them there.

## Command Reference

### Linting

```bash
# plan tags
python3 .github/scripts/check_plan_tags.py
```

CI runs it in the `content_check` job of the root `.github/workflows/ci.yaml`. There is no local build. Instead, a pull request into `main` from a branch of the mirrord repository gets a `GitBook (./docs) - metalbear.com/mirrord/docs/` status whose Details link opens a preview of the site. Only people with a GitBook account can view the preview. A pull request from a fork gets no preview: GitBook serves previews from the docs site's domain, so a fork's preview would put content that anyone can write under `metalbear.com`, and the spaces' Git Sync settings keep fork previews off for that reason.

## Publishing

GitBook imports each commit on `main` that changes this directory, and reports the result as two commit statuses, `GitBook (./docs)` and `GitBook (./docs) - metalbear.com/mirrord/docs/`. The Guides space syncs the root `guides/` directory the same way, with the project directory `guides` and the statuses `GitBook (./guides)` and `GitBook (./guides) - metalbear.com/mirrord/docs/guides/`. No CI job waits for these statuses, so a failed import shows only as a failed GitBook status on the merge commit.

A commit that GitBook never imports leaves the site at the previous import. Check the space's Git Sync in GitBook, and try syncing it again from there. GitBook's Git Sync troubleshooting guide also says that GitBook imports all the content again when a new commit reaches the branch, so the next merge that changes `docs/` (or `guides/`) also brings in the changes of the failed import.

To take a page off the site, merge a revert of its pull request.

If GitBook can't sync `main`, point a space's Git Sync back at `docs-published` or `guides-published`, with no project directory, and choose GitHub → GitBook as the initial sync direction. The site then returns to how it was before the spaces synced `main`, without the pages merged since. The other direction would export the space's content, `main`'s pages included, onto that branch.

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
