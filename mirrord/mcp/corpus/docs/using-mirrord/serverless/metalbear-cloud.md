---
title: "MetalBear Cloud Sessions-Manager"
description: "Target serverless workloads such as Amazon ECS tasks without the mirrord Operator, through a sessions-manager hosted by MetalBear, with workloads authenticated by a MetalBear API key and developers by `mirrord login`."
tags:
  - experimental
  - enterprise
---

# MetalBear Cloud Sessions-Manager

{% hint style="warning" %}
The MetalBear Cloud sessions-manager is **experimental**, and may change without notice.
{% endhint %}

In the MetalBear Cloud [deployment mode](README.md#deployment-modes), sessions-manager is hosted by MetalBear. There's nothing to install in your infrastructure, and you don't need a Kubernetes cluster or the mirrord Operator. Workloads and developers connect to it directly over HTTPS. Workloads authenticate with a MetalBear API key, and developers with their MetalBear account, through `mirrord login`. At a glance, the setup is:

1. Get an API key ([API keys](#api-keys)).
2. Allow outbound HTTPS from your workloads to MetalBear ([Network access](#network-access)).
3. Connect each workload, which sets the API key on it ([Connecting workloads](#connecting-workloads)).
4. Have developers sign in with `mirrord login` ([Developer setup](#developer-setup)).

## Prerequisites

1. An [API key](README.md#api-key), `<API_KEY>`.
2. Outbound HTTPS access from your workloads and from developers' machines to MetalBear.

---

## How it works

{% hint style="info" %}
This section is for whoever reviews the setup, such as your security team. To start the setup, skip to [API keys](#api-keys).
{% endhint %}

```
 developer machine                     MetalBear                                workload network
┌───────────────────┐   HTTPS     ┌──────────────────────────┐   HTTPS     ┌───────────────────────────┐
│ mirrord exec      │───────────► │ sessions-manager         │ ◄───────────│ application container     │
│ (local process)   │             │                          │             │ + mirrord remote          │
└─────────┬─────────┘             │ app.metalbear.com        │             │   bootstrap (LD_PRELOAD)  │
          └─────────────────────► │   (token exchange)       │ ◄───────────┴───────────────────────────┘
                                  └──────────────────────────┘
```

Workloads authenticate with the API key:

1. The API key is set on the workload, in `MIRRORD_SESSIONS_MANAGER_API_KEY`.
2. Before each request that registers with sessions-manager, the workload exchanges the key at `https://app.metalbear.com` for a short-lived token, and presents the token to sessions-manager as `Authorization: Bearer <token>`. The key itself is only ever sent to that exchange, in the request body, over HTTPS, and never to sessions-manager. Neither the key nor the tokens are logged.
3. Sessions-manager verifies the token against MetalBear's signing keys. The token identifies the organization that owns the key.

Developers authenticate with their MetalBear account:

1. `mirrord login` signs the developer in, and stores a token on their machine that names them and the organization they signed in to.
2. When the developer starts a session, mirrord presents that token to sessions-manager as `Authorization: Bearer <token>`. The developer never handles the API key.
3. Sessions-manager verifies the token against the same signing keys, and only accepts tokens issued by `https://app.metalbear.com`.

Each credential is accepted only for its own side of a session: the token exchanged for the API key can only register a workload, and a `mirrord login` token can only register a developer. Every registration is scoped to the organization its token names, so workloads and developers are only ever paired within the same organization.

Each data-plane connection is bound to its session by a single-use credential delivered with the session assignment, so a caller can't attach to another caller's session.

Only API keys issued for serverless workloads are accepted. Other MetalBear credentials, such as the Operator's license key, are refused by sessions-manager.

Both sides only make outbound HTTPS connections. Neither needs a network path to the other, and nothing in your infrastructure accepts inbound connections.

{% hint style="warning" %}
The API key authenticates your organization, not a specific workload: anyone who holds it can register a workload under any service and environment of your organization, and be paired with your developers' sessions. It can't be used to start a developer session. Treat it as a secret, and give it only to the workloads that need it.
{% endhint %}

---

## API keys

Generate the API key for your organization at [app.metalbear.com](https://app.metalbear.com). It begins with `metalbear_key_`; mirrord refuses to start with a value that doesn't.

Only workloads use the key; developers don't need it. Store it in a secret store, such as AWS Secrets Manager, rather than in plain configuration or source control. Your platform's connection guide shows how to inject it into the workload, for example [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md#store-the-api-key).

To rotate the key, generate a new one, update it wherever it's stored, and restart the workloads, then revoke the old one. Once a key is revoked, new registrations with it are refused; sessions that are already open continue until they end.

## Network access

Both workloads and developers' machines need outbound HTTPS (TCP 443) to:

| Host | Used for |
| --- | --- |
| `sm-aws-use1.app.metalbear.com`, MetalBear's sessions-manager | Registering, and the session's traffic. |
| `app.metalbear.com` | Exchanging the API key for a token, and `mirrord login`. |

MetalBear may add sessions-managers in other regions under `app.metalbear.com`. To allow them without another firewall change, allow `*.app.metalbear.com` alongside `app.metalbear.com` rather than the exact sessions-manager host. The wildcard doesn't match `app.metalbear.com` itself, so it's still needed.

Sessions hold their connections open for as long as the developer runs them. Any proxy or firewall on the path must not buffer responses and needs an idle timeout of at least 300 seconds.

## Connecting workloads

Each workload needs one variable, next to the ones that [name the workload](ecs.md#name-the-workload):

| Variable | Value |
| --- | --- |
| `MIRRORD_SESSIONS_MANAGER_API_KEY` | `<API_KEY>`, injected from your secret store. |

The bootstrap connects to MetalBear's sessions-manager, at `https://sm-aws-use1.app.metalbear.com/sm`, when no other variable selects one. Don't set `MIRRORD_OPERATOR_API_URL` or `MIRRORD_OPERATOR_KUBE_CONTEXT`, which select the [Operator-hosted mode](operator-hosted.md) instead, or `MIRRORD_SESSIONS_MANAGER_URL`, which points the bootstrap at a different sessions-manager.

How to set them depends on the platform:

* [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md)

## Developer setup

Developers need no Kubernetes access and no API key. They sign in to their MetalBear account once:

```bash
mirrord login
```

mirrord stores the credentials on their machine, in `~/.mirrord/auth.json`, and uses them for every MetalBear Cloud session. Then they use the `mirrord.json` from [Using mirrord with a Serverless Workload](README.md#using-mirrord-with-a-serverless-workload) without `"operator": true`. With `"operator": true`, mirrord connects through the Operator instead. When the session starts, mirrord prints `Connecting through the MetalBear Cloud sessions-manager at https://sm-aws-use1.app.metalbear.com/sm`; if it names another sessions-manager, see [Which sessions-manager mirrord connects through](README.md#which-sessions-manager-mirrord-connects-through).

* **Several organizations:** a developer who belongs to more than one organization must sign in to the one that owns the workload's API key. When they've signed in to several, mirrord uses the most recent `mirrord login`. To switch, run `mirrord login` again and pick the other organization.
* **Expiry:** the sign-in isn't renewed automatically. Once it expires, mirrord warns when a session starts, and new sessions are refused until the developer runs `mirrord login` again. A session that's already running keeps working, and picks up a new sign-in when it reconnects, without a restart.
* **No API key:** mirrord doesn't use `MIRRORD_SESSIONS_MANAGER_API_KEY` on a developer's machine. Remove it from shells and IDE run configurations that set it.

---

## Troubleshooting

The workload's messages appear in the application container's logs, and the developer's in mirrord's output. To see which sessions-manager a workload registers with, and why, set `MIRRORD_AGENT_RUST_LOG=info` on it. The bootstrap then logs its choice when it starts, for example `Registering with the MetalBear Cloud sessions-manager, since neither MIRRORD_OPERATOR_API_URL nor MIRRORD_SESSIONS_MANAGER_URL is set`.

| Symptom | Likely cause |
| --- | --- |
| `MIRRORD_SESSIONS_MANAGER_API_KEY must hold a MetalBear API key, which begins with metalbear_key_` | The variable holds something else, such as the secret's ARN instead of its value, or has extra whitespace or quotes. |
| `the MetalBear API key was rejected` | The key is mistyped or revoked. |
| `HTTP control plane returned 401 Unauthorized` in the workload's logs | `MIRRORD_SESSIONS_MANAGER_API_KEY` isn't set, or the key isn't a serverless workload key, for example an Operator license key. |
| A connection timeout to sessions-manager or `app.metalbear.com` | Outbound HTTPS is blocked ([Network access](#network-access)). |
| The workload registers but the developer is never paired | The workload's service and environment names don't match `target.path` and `target.namespace` in `mirrord.json`, or the developer's latest `mirrord login` is to a different organization than the workload's API key belongs to ([Developer setup](#developer-setup)). |
| `You are not logged in to https://app.metalbear.com, so a sessions-manager that authenticates its users will refuse this session` | The developer hasn't run `mirrord login`. Run `mirrord login`. |
| `Your login to https://app.metalbear.com expired at <time>` | The developer's sign-in expired, and isn't renewed automatically. Run `mirrord login`. |
| `Could not read your login to https://app.metalbear.com from ~/.mirrord/auth.json` | mirrord can't read the file, for example because of its permissions, and starts the session without a sign-in, which is refused. Check its permissions, or run `mirrord login` again. |
| `HTTP control plane returned 401 Unauthorized` in mirrord's output on a developer's machine | The developer isn't signed in, their sign-in expired, or `~/.mirrord/auth.json` can't be read. One of the warnings above appears first. Run `mirrord login`. |
| `sessions-manager refused your mirrord login, which has expired, been revoked, or is for another organization` | The sign-in was sent but refused, for example because it was revoked. Run `mirrord login`. |
| `MIRRORD_SESSIONS_MANAGER_URL, MIRRORD_OPERATOR_API_URL and MIRRORD_OPERATOR_KUBE_CONTEXT are mutually exclusive` | The workload sets variables of both modes; keep only those of [Connecting workloads](#connecting-workloads). |
| mirrord prints `Connecting through the sessions-manager hosted by the mirrord operator` | `mirrord.json` sets `"operator": true`. Remove it ([Developer setup](#developer-setup)). |
| mirrord prints `Connecting through the sessions-manager at <URL>` | `sessions_manager.url` is set in `mirrord.json`, or `MIRRORD_SESSIONS_MANAGER_URL` in the developer's shell or IDE run configuration. Unset both. |
| `the sessions-manager URL is invalid` | `sessions_manager.url` or `MIRRORD_SESSIONS_MANAGER_URL` holds something other than a URL. Unset both to use MetalBear Cloud. |
