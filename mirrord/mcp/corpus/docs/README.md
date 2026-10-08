---
title: Welcome
description: Connect local code, run by developers or AI coding agents, to your live Kubernetes cluster
layout:
  title:
    visible: true
  description:
    visible: true
  tableOfContents:
    visible: true
  outline:
    visible: true
  pagination:
    visible: true
---

mirrord is a tool that lets developers and AI coding agents run local code as if it were running inside a Kubernetes cluster, without building container images or deploying. Your code still runs locally, but mirrord proxies incoming and outgoing traffic, environment variables, and files back and forth between your local process and the cluster.

The result is that your local process behaves as if it's running in the cloud, even though it's still running on your machine. Use mirrord to read from the live cluster (things like real env vars, real service responses, real queue contents) when writing or generating code, so your code is grounded in what's actually deployed. Then run it against those same services once it's written, and find out immediately whether it works instead of waiting on CI and staging.

This works for a single service, or for several at once with [`mirrord up`](using-mirrord/multiple-concurrent-sessions.md), which runs multiple mirrord sessions from a single config file.

AI coding agents like Claude Code, Cursor, and Codex can [use mirrord](./using-mirrord-with-ai/README.md) the same way: reading the cluster to generate code grounded in real env vars, service responses, and queue contents, and then running the code against real cluster services to catch integration issues and iterate until it works end-to-end.

<table data-view="cards" data-card-size="large"><thead><tr><th></th><th></th><th data-hidden data-card-target data-type="content-ref"></th><th data-hidden data-card-cover data-type="files"></th></tr></thead><tbody><tr><td><strong>Quick Start</strong></td><td>Install mirrord and run your first session in minutes.</td><td><a href="getting-started/quick-start.md">getting-started/quick-start.md</a></td><td><a href=".gitbook/assets/card-quick-start.png">card-quick-start.png</a></td></tr><tr><td><strong>What is mirrord?</strong></td><td>Understand how mirrord works and how it's different from alternatives.</td><td><a href="getting-started/what-is-mirrord.md">getting-started/what-is-mirrord.md</a></td><td><a href=".gitbook/assets/card-what-is-mirrord.png">card-what-is-mirrord.png</a></td></tr><tr><td><strong>Use Cases</strong></td><td>Remocal development, CI/CD integration, using mirrord with AI agents, and more.</td><td><a href="use-cases/local-development.md">use-cases/local-development.md</a></td><td><a href=".gitbook/assets/card-use-cases.png">card-use-cases.png</a></td></tr><tr><td><strong>Guides</strong></td><td>Step-by-step guides for debugging microservices in Kubernetes with mirrord.</td><td><a href="https://metalbear.com/mirrord/docs/guides/">https://metalbear.com/mirrord/docs/guides/</a></td><td><a href=".gitbook/assets/card-guides.png">card-guides.png</a></td></tr></tbody></table>

## Open source vs Teams

mirrord's core functionality is free and open source. [mirrord for Teams](https://metalbear.com/mirrord/pricing) adds the mirrord Operator, enabling concurrent cluster usage, advanced traffic control, and organizational governance.
