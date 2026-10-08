# mirrord-chaos

Chaos test your app with mirrord: inject artificial latency or connection errors into a mirrord session's **outgoing traffic** via per-session chaos rules, managed with the `mirrord chaos` CLI.

## What it does

This skill helps AI agents:
- **Generate** valid chaos rule JSON files (selector + effect + priority)
- **Manage** rules with `mirrord chaos`: create, list, get, modify, delete, clear
- **Wire** chaos into CI: rule files checked into the repo, session lookup by `--key`, rules applied with `mirrord chaos add`, teardown after the test run
- **Explain** rule semantics: per-session scoping, highest-priority-wins, percentage, TCP-only selectors today
- **Troubleshoot** chaos-specific issues (validation rejections, rules that never fire, sessions not found)

## Two modes

1. **Interactive**: a `mirrord exec` session, rules managed with `mirrord chaos add/list/edit/delete`.
2. **CI**: rule files checked into the repo, applied by the pipeline before the test run, torn down after.

## Example prompts

```
"Add 750ms latency to my service's database connections with mirrord"

"Make 30% of calls to the payments API time out"

"How do I create and delete mirrord chaos rules?"

"Run my integration tests with injected connection errors in CI"

"How do I use mirrord chaos?"
```

## Key commands

```bash
mirrord chaos add -s "$SESSION_ID" -f latency-rule.json
mirrord chaos list -s "$SESSION_ID"
mirrord chaos edit -s "$SESSION_ID" -r "$RULE_ID" -f latency-rule.json
mirrord chaos delete -s "$SESSION_ID" -r "$RULE_ID"
```

## Prerequisites

- mirrord **CLI 3.241.0+** (`mirrord chaos` starts the local UI server silently if needed)
- A live mirrord session to attach rules to
- Chaos testing is an **alpha** feature

## Learn more

- [Chaos Testing docs](https://metalbear.com/mirrord/docs/use-cases/chaos-testing)
- [Local UI docs](https://metalbear.com/mirrord/docs/using-mirrord/local-ui)
