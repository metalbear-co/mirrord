# mirrord docs

## Overview

This repository holds the Documentation tab of the [mirrord docs](https://metalbear.com/mirrord/docs): GitBook builds it from the Markdown under `docs/`, and the site follows `main`, so merging publishes. A page for an unreleased change names the versions expected to ship it, and its PR merges once they are out.

The Configuration tab is generated from the doc comments in the mirrord repository's `mirrord/config/src`, and the Changelog tab from each product's `CHANGELOG.md`. Fix them there.

## Command Reference

### Linting

```bash
# plan tags
python3 .github/scripts/check_plan_tags.py
```

There is no local build. GitBook renders each PR against `main`, and its `GitBook - metalbear.com/mirrord/docs/` check links a preview of the whole site.

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
