---
title: Agent-Started Trials
description: How an AI coding agent starts a mirrord for Teams trial, and how to claim the organization it creates
draft: false
toc: true
tags: ["team", "enterprise"]
---

Some things an agent needs on a shared cluster only work with mirrord for Teams: branching a database so its writes don't reach everyone else's data, splitting a queue so it doesn't eat messages other people need, or stealing traffic from a target another session already holds.

On a cluster with no license, an agent that follows [metalbear.com/agents.md](https://metalbear.com/agents.md) can start a trial itself rather than stopping and waiting for you to sign up. It ends up with a working cluster and you end up with a link to claim. If you already have an organization, the agent installs with it instead (see [If you already have an organization](#if-you-already-have-an-organization)), so no second organization gets created.

## What the agent does

It runs two commands against the cluster of its current kubecontext, or of the one it names with `--context`. No authentication, no credit card, and no API key for the agent to handle:

```bash
mirrord operator signup
mirrord operator install --from-signup
```

`mirrord operator signup` starts the trial and prints its end date and the claim URL for the agent to hand you. It saves the trial's API key on this machine instead of printing it, then prints the install command to run next. It also sends the cluster's ID (the UID of its `default` namespace) along with the signup, if it can read it, and MetalBear stores it with the trial. `--cluster-hint <name>` sends a different name, and `--no-hint` sends none.

`mirrord operator install --from-signup` installs the Operator with the saved key. In a terminal, it first asks you to confirm the kubecontext and the namespace that it installs the Operator into, before it changes the cluster. `--yes` skips the question. An agent usually runs the command without a terminal, and then it doesn't ask. If the installation fails or is stopped, retry without starting another trial: remove what was installed with `mirrord operator uninstall`, then run `mirrord operator install --from-signup` again. If the signup used `--context`, pass the same `--context` to the uninstall and the install.

`mirrord operator install` without `--from-signup` or `--api-key` doesn't start a trial. It logs in to an existing organization instead (see [If you already have an organization](#if-you-already-have-an-organization)).

The trial is a **provisional organization** carrying an Enterprise trial license, good for seven days from the signup.

The Operator is installed from the default Helm chart, without needing Helm itself. If an Operator is already installed in the cluster, or an earlier installation left objects behind, the command stops and says so rather than touching anything, and tells you how to remove them with `mirrord operator uninstall`. For anything beyond the default installation, install with the [Helm chart](../managing-mirrord/operator.md#helm) instead.

### Calling the signup endpoint directly

Without the mirrord CLI, an agent can post to the signup endpoint itself:

```bash
curl -fsS -X POST https://app.metalbear.com/api/v1/agent/signup \
  -H 'content-type: application/json' \
  -d '{"agent": "claude-code", "developer_email": "you@example.com", "cluster_hint": "staging"}'
```

`developer_email` and `cluster_hint` are optional and unverified, and MetalBear stores them with the trial.

The response describes the provisional organization:

```json
{
  "organization_id": "...",
  "api_key": "...",
  "license_type": "enterprise-trial",
  "trial_ends_at": "2026-09-17T12:00:00+00:00",
  "claim_code": "mbclaim_...",
  "claim_url": "https://app.metalbear.com/claim?code=mbclaim_...",
  "instructions_url": "https://metalbear.com/agents.md"
}
```

The agent installs the Operator with that key as `cloud.apiKey.key` (see [Cloud API key](../managing-mirrord/operator.md#cloud-api-key)), then gives you the `claim_url`.

Because the trial is an Enterprise license, it also covers the features a Team license doesn't, including [Preview Environments](../use-cases/preview-environments.md). The Helm chart enables them by default through `operator.previewEnv`, so an Operator installed by `mirrord operator install` supports them as is.

## Claiming the organization

Open the claim URL. You can sign in with an existing account or create one on the spot; the link survives either, including the verification mail that account creation sends you. What happens next depends on the account you use.

| You sign in as | Result |
| --- | --- |
| A new account with no organization | A new organization is created for you, keeping the trial's expiry date and everything the agent already did |
| An admin of an existing organization | The agent's cloud API key moves into your existing organization |
| A member of an existing organization who is not an admin | Rejected. Ask an admin to open the link |

Claiming also deletes the provisional organization, so the cluster keeps working against your real one without reinstalling anything. A new organization inherits the agent's history along with the license, so the getting-started checklist already counts the Operator as installed and your usage shows the sessions the agent ran before you claimed. An organization that already existed keeps its own license, and so its own history.

An existing organization that already holds an active Operator cloud API key will reject the claim rather than replace the key it has. **Revoke** under **API Keys** replaces the key with a new active one, so it doesn't clear the conflict. Instead, move the cluster onto your organization: run `mirrord operator uninstall`, then `mirrord operator install --api-key <key>` with your organization's key. Read-only keys are a different scope and don't collide, so they can stay.

## Until it is claimed

A provisional organization is not a stuck state for the agent. The trial license works, so the cluster is usable straight away. What is missing is a person:

- **Nobody can administer it.** Billing, seats, and members all need a human owner, and the first person to claim it becomes that owner.
- **It expires with the trial.** An unclaimed organization reaches `trial_ends_at` with nobody able to renew or convert it.

Claim codes are single-use. Once one has been claimed, opening the same link from a different organization fails rather than silently joining it. Reopening it as the same admin who claimed it is harmless.

## Removing the Operator

`mirrord operator uninstall` removes an Operator that `mirrord operator install` installed, also after an installation that failed half-way. Like the installation, it doesn't need Helm:

```bash
mirrord operator uninstall
```

It first lets the Operator end its sessions, so that the workloads they changed are restored, and then deletes everything that the installation created. That includes the mirrord CRDs, so it also deletes the mirrord policies and profiles of the cluster. Like `mirrord operator install`, it takes `--context`, asks for confirmation in a terminal, and doesn't ask with `--yes` or without a terminal.

If you moved the installation to Helm, remove it with `helm uninstall` instead. `mirrord operator uninstall` stops and tells you so.

## If you already have an organization

You don't need a trial. Run `mirrord operator install`, or let the agent run it: it asks you to approve the login to [app.metalbear.com](https://app.metalbear.com) in the browser (or reuses a `mirrord login` from the same machine), creates your organization's operator API key, and installs the Operator with it, without showing the key. If no browser opens, it prints the URL to visit, and it waits up to five minutes for the approval.

- Only an admin of the organization can create the key, and only while the organization has no operator API key yet. If it already has one, install with `mirrord operator install --api-key <key>`, using the key from a cluster that already runs the Operator, or ask an admin for it.
- If the login page says the account has no organization yet, finish setting it up at [app.metalbear.com](https://app.metalbear.com), then run the command again.
- If the installation fails after the key was created, remove what was installed with `mirrord operator uninstall`. The created key stays active, so running the command again fails: under **API Keys**, use **Revoke** to replace the key and copy the new one, then install with `mirrord operator install --api-key <key>`.

You can also generate a cloud API key under **API Keys** and install with `mirrord operator install --api-key <key>`, or install the Operator yourself following the [dashboard setup guide](../managing-mirrord/dashboard/cloud.md). Signing up again creates a second organization you then have to clean up.
