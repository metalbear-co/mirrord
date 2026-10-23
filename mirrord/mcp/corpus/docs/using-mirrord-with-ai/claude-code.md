---
title: How to Let Claude Code Test Its Changes Against Your Staging Cluster
description: Set up Claude Code to run the code it writes against the real services in your staging Kubernetes cluster with mirrord, with no deploy and no mocks to maintain.
draft: false
toc: true
tags: ["oss", "team", "enterprise"]
---

Claude Code can write a change to a microservice in seconds or minutes. To check that the change works, it needs the things that the service talks to: the other services, the queues, the database, and the configuration of the real environment. On a laptop, those are missing or mocked, so Claude Code can only check that the code compiles and that the unit tests pass.

mirrord closes that gap. Claude Code runs the service on your machine, and mirrord connects the process to a pod in your staging cluster. The process gets the pod's environment variables, DNS, network access, and incoming traffic. Claude Code can then run the service and its tests against the real services in staging, read the real responses, and fix what breaks before you review the change. Nothing is built or deployed. Claude Code can also start the service with mirrord and leave it running, so that you can test the change yourself through staging, for example from the staging frontend with your header. With mirrord for Teams, it can also create a [preview environment](../use-cases/preview-environments.md) for a pull request that you and your colleagues can open.

This also works when many developers and Claude Code sessions test against the same staging cluster at the same time. With the mirrord Operator, each session gets only its own requests, its own queue messages, and its own database branch, so the sessions do not break each other or the deployed services.

This guide sets this up for one service in about five minutes. To set up many services at once, or to use an agent other than Claude Code, use the [meta-prompt](./the-meta-prompt.md).

## What you need

- Claude Code.
- A kubeconfig with access to a **staging or development** cluster.
- A service that runs in that cluster, and its source code on your machine.

## 1. Install the mirrord CLI

{% tabs %}
{% tab title="macOS/Linux" %}

```sh
brew install metalbear-co/mirrord/mirrord
```

{% endtab %}
{% tab title="Windows" %}

```powershell
choco install mirrord
```

{% endtab %}
{% endtabs %}

For other install methods, see [Installing the mirrord CLI](../installing-mirrord/cli.md).

Check the install with `mirrord --version`, and check cluster access with `kubectl get pods -n <namespace>`.

## 2. Add the mirrord skills to Claude Code

In Claude Code, run:

```
/plugin marketplace add metalbear-co/skills
/plugin install mirrord@mirrord-skills
```

The plugin gives Claude Code skills for mirrord: how to find a target, write a valid `mirrord.json`, run a session, and use the features for shared clusters. Claude Code loads the right skill when your prompt needs it. See [Agent Skills for mirrord](./ai-skills-plugin.md) for the full list.

## 3. Tell Claude Code how to test in this repository

{% hint style="warning" %}
If the integration tests write to or delete data in a database that other people use, do not run them against the shared database. Use [DB branching](../sharing-the-cluster/db-branching.md) to give the session its own copy, or leave the test line out.
{% endhint %}

Add a short section to the `CLAUDE.md` file at the root of the repository. Claude Code reads this file at the start of each session.

```markdown
## Testing against staging

ALWAYS test changes to <service> against the staging cluster with mirrord before you report them as done.

- Target: deployment/<service> in namespace <namespace>, kubecontext <staging-context>
- Run the service: mirrord exec -f .mirrord/mirrord.json -- <command that starts the service>
- Run the integration tests (if the service has them): mirrord exec -f .mirrord/mirrord.json -- <test command>
- Send test requests with the header `baggage: mirrord-session=<your-name>`, so that only your requests reach your local process.
- NEVER target a production cluster.
```

## 4. Propagate the header (if requests reach the service through other services)

The header filter in step 5 matches only the requests that reach the target with the header. If those requests come from other services, such as a frontend or an API gateway, those services must copy the `baggage` header onto the calls they make. This is [header propagation](../sharing-the-cluster/header-propagation.md). Many services already propagate `baggage`, because OpenTelemetry and other tracing libraries forward it by default. Skip this step if your services already propagate it, or if you send requests to the service directly.

The `mirrord-header-propagation` skill from step 2 sets it up. Ask Claude Code:

```
Use the mirrord-header-propagation skill to propagate the baggage header across my services.
```

## 5. Let Claude Code write the mirrord config

The `mirrord-config` skill from step 2 writes `mirrord.json` and validates it against the schema. Ask Claude Code to use it:

```
Use the mirrord-config skill to create .mirrord/mirrord.json for <service>.
Target deployment/<service> in namespace <namespace>, and pin the kubecontext to
<staging-context>. Steal only the requests whose baggage header has the entry
mirrord-session=<your-name>, and not mirrord-session=<your-name>2. Read env vars
and files from the pod.
```

Two settings in the prompt keep the session safe on a shared cluster:

- **The kubecontext.** The config pins the session to the staging cluster. Without it, mirrord uses your current kubecontext, so the same command runs against another cluster after you switch contexts.
- **The header filter.** Only requests that carry your `baggage` entry go to the process on your machine. All other requests keep going to the deployed service, so your session does not break staging for the rest of the organization.

If you only want to read traffic and not answer it, ask for `mirror` mode instead of steal.

## 6. Let Claude Code run mirrord without a prompt each time

Claude Code asks for approval before it runs a shell command. To let it run mirrord sessions without a prompt each time, add this to `.claude/settings.json` in the repository:

```json
{
  "permissions": {
    "allow": ["Bash(mirrord exec:*)", "Bash(mirrord ls:*)"]
  }
}
```

## 7. Ask Claude Code to make and test a change

Some prompts to start with:

- "Add a `currency` field to the `/orders` response. Then run the service with mirrord and call `/orders` on staging with my header to check the new field."
- "Run the integration tests for this service against staging with mirrord, and fix what fails."
- "Before you change the payment client, run the service with mirrord and print the real response from the payments service, so you see the actual schema."

Claude Code runs `mirrord exec`, sends requests with your header, reads the responses and logs, and changes the code until the result is correct. The first session on a cluster takes longer, because mirrord starts its agent pod.

## More than one developer or agent on the same cluster

Open-source mirrord works on any cluster. When several developers or several Claude Code sessions test against the same service at the same time, the cluster needs the mirrord Operator, which is part of mirrord for Teams. With the Operator, each session gets only the requests that match its own header filter, and it can also get:

- its own branch of a staging database ([DB branching](../sharing-the-cluster/db-branching.md)), so its writes do not change the shared data
- only its own messages from a shared queue ([queue splitting](../sharing-the-cluster/queue-splitting.md)) for Kafka, SQS, RabbitMQ, and other brokers
- a [preview environment](../use-cases/preview-environments.md) that keeps the change running after the session ends, for review

If the cluster has no license, Claude Code can start a seven-day Enterprise trial itself with `mirrord operator signup`, install the Operator with `mirrord operator install --from-signup`, and give you a link to claim it. If you already have an organization, it installs with that instead: `mirrord operator install`, run by an organization admin, logs in and creates the key, and `--api-key <key>` uses the key your organization already has. See [Agent-Started Trials](./agent-started-trials.md).

## If a session does not start

- **Claude Code cannot connect to the mirrord agent.** Check that `kubectl get pods -n <namespace>` works from the same terminal. Then check that you can create pods in the namespace where mirrord starts its agent. Without the Operator, that is the current namespace of your kubecontext, not the target's namespace, unless you set `agent.namespace` in the config. Tell Claude Code to stop and report the error after two failures, not to retry in a loop.
- **The requests do not reach the local process.** Check that the requests carry the `baggage` entry that `header_filter` matches. The filter is a regular expression, matched against the request headers. If the requests come through other services, check that those services [propagate the header](../sharing-the-cluster/header-propagation.md).
- **The service reads a file that is not on your machine.** mirrord reads files from the pod by default with `"fs": "read"`. Change the `fs` setting if the service must read local files.

For other errors, see [Common Issues](../troubleshooting/common-issues.md).

## Related

- [Agent Skills for mirrord](./ai-skills-plugin.md)
- [Configure AI agents to use mirrord](./the-meta-prompt.md) (the meta-prompt)
- [metalbear.com/agents.md](https://metalbear.com/agents.md), instructions written for agents
- [mirrord integrations](https://metalbear.com/mirrord/integrations/), setup for Codex, Cursor, GitHub Copilot, and other agents
