---
name: running-ci-in-mirrord
description: Use when you push to, wait on CI for, or retrigger CI on a pull request in the mirrord repo.
metadata:
  accomplish_note: "Skill created by Accomplish on behalf of Han Kim on 2026-11-27"
---
# CI on mirrord pull requests
- A push to a pull request whose base branch is not `main`, such as an upper part of a stack, starts no CI run.
- To start CI on such a pull request, close and reopen it: `gh pr close <n> --repo metalbear-co/mirrord && gh pr reopen <n> --repo metalbear-co/mirrord`.
- After a push, confirm a run exists for the new head commit (`gh api "repos/metalbear-co/mirrord/actions/runs?head_sha=<sha>"`) before reading `gh pr checks` as the CI result.
