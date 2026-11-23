# Content to add to the AI section of the docs

> This file is NOT a guide. It contains the "why" content extracted from the guides that should be added to the [Using mirrord with AI Agents](https://metalbear.com/mirrord/docs/using-mirrord-with-ai) docs page (or a new sibling page). Delete this file after the content has been moved.

---

## Why test AI-generated code against real services

AI coding tools generate code in minutes. But validating that code still takes hours, waiting on CI/CD pipelines, staging deploys, and environment provisioning. The bottleneck has shifted from writing code to proving it works.

### The validation bottleneck

The typical AI-assisted workflow looks like this:

1. AI generates code in 2-5 minutes
2. Developer pushes to a branch
3. CI builds a container image (5-10 minutes)
4. CI deploys to a test environment (5-10 minutes)
5. E2E tests run against the deployed service (5-15 minutes)
6. Tests fail, back to step 1

A single iteration takes 20-40 minutes. If the AI-generated code needs 3 rounds of fixes, you've spent 1-2 hours validating what took 5 minutes to write.

### Why mocks don't solve this

The instinct is to skip the deploy step and test locally against mocks. But AI agents are uniquely bad at working with mocks:

**AI doesn't know when mocks are stale.** A human developer who wrote a mock has a mental model of what the real API returns. An AI agent has no such context, it treats the mock as truth, even when the mock diverges from reality.

**AI can hallucinate mock responses.** When an AI writes tests, it can generate mock responses that look plausible but don't match your actual backend. The tests pass beautifully against responses that would never come from your real services.

**Mocks hide integration bugs.** Unit tests verify isolated logic. Integration tests verify that services work together. The bugs that matter most in microservice architectures, schema mismatches, auth failures, timeout behavior, queue delivery semantics, are integration bugs that mocks can't surface.

### What real services catch that mocks miss

- **Schema drift**: your AI-generated code expects a field that was renamed last week
- **Auth changes**: a new permission was added that the mock doesn't enforce
- **Performance regressions**: real latency reveals timeouts that 0ms mock responses hide
- **Data edge cases**: real data has nulls, Unicode, and unexpected formats that seed data doesn't
- **Service version mismatches**: the downstream service was updated and its response format changed

### The inner loop: from hours to seconds

| Step | Without mirrord | With mirrord |
|------|----------------|--------------|
| Code generation | 2-5 min | 2-5 min |
| Build container | 5-10 min | **Skipped** |
| Deploy to staging | 5-10 min | **Skipped** |
| Run tests | 5-15 min | 10-30 sec |
| Fix and re-test | 20-40 min per round | **Seconds** |
| **Total (3 iterations)** | **1-2 hours** | **10-15 min** |

### Why test after every change

Without guardrails, an agent makes change after change on top of a bad assumption. By the time it runs tests, errors have compounded and it's impossible to tell which change caused the break.

With E2E tests after every change:

- The agent catches breaks immediately
- Each fix is small and targeted
- The agent stays on track instead of spiraling
- Less time spent unwinding cascading errors

### CI/CD comparison

The [MetalBear playground](https://github.com/metalbear-co/playground) runs the same E2E tests two ways in CI/CD:

| | Traditional (kind cluster) | mirrord (existing staging) |
|--|---------------------------|---------------------------|
| Timeout | 45 minutes | 15 minutes |
| Setup | Create cluster, build images, deploy all services | Install mirrord, configure kubeconfig |
| Test execution | Tests hit locally-deployed services | Tests hit real staging services |
| Teardown | Delete kind cluster | Stop mirrord session |
| Infrastructure cost | Ephemeral compute for full cluster | Zero, uses existing staging |
