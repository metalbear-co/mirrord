---
title: Logging in with SSO
description: Log in to the mirrord Operator through your company's identity provider with mirrord login, run mirrord sessions and the IDE through that login, pick targets and namespaces without a kubeconfig, see your logins with mirrord whoami, and remove one with mirrord logout.
tags:
  - enterprise
---

# Logging in with SSO

When your team's mirrord Operator has [single sign-on](../managing-mirrord/sso.md) turned on, you log in to it through your company's identity provider instead of a kubeconfig. One login lasts 24 hours, and mirrord uses it for every command that talks to that Operator until it expires or you log out.

You need mirrord `3.292.0` or later and an Operator of version `3.239.0` or later with an Enterprise license. Your admin gives you the Operator's URL, the `sso.url` value of its Helm chart.

## Logging in

```bash
mirrord login --url https://mirrord.corp.example.com
```

mirrord opens your browser at your identity provider. Once you have signed in there, the browser shows a page saying you are logged in and the command prints who you are logged in as and until when:

```text
Logged in to mirrord.corp.example.com as dev@corp.example.com. The login is valid until 2026-11-20 09:12. mirrord uses this operator now.
```

Before the browser opens, mirrord checks the URL. An Operator without SSO, or one older than `3.239.0`, is refused with a message naming what is missing. An Operator whose license does not include SSO is refused with the same error every paid feature gives, naming SSO and the Enterprise plan. A URL that is neither an Operator nor mirrord Cloud is refused too, so a typo never sends you to a login page it should not.

If your browser does not open, the command prints the link to open by hand. The login finishes when the browser reaches mirrord's page on your machine, so both must run on the same computer.

The same command logs you in to mirrord Cloud when the URL is the Cloud's, `https://app.metalbear.com` by default. A Cloud login lives next to your Operator logins and never changes which Operator mirrord uses.

### Several Operators

You can be logged in to several Operators at once, for example production and staging. mirrord uses the one you logged in to last. To switch to another one you are still logged in to, run `mirrord login --url` with its URL again: when that login has not expired, mirrord switches without opening a browser.

```text
Logged in to staging.mirrord.corp.example.com as dev@corp.example.com already, valid until 2026-11-20 09:12. mirrord uses this operator now.
```

## Running mirrord while logged in

While a login is in use, every command that talks to the Operator goes to its URL with that login, and mirrord does not read your kubeconfig at all. That covers `mirrord exec` with every Operator feature it reaches (a target, a copied target, a profile, database branching, queue splitting), the IDE extensions, listing sessions with `mirrord operator status`, and ending them with `mirrord operator session`. Your identity comes from the login, so RBAC sees your SSO user and groups, and the Operator records the session with `sso` in the `AUTH` column of `mirrord operator status`:

```text
Session ID  Target               Namespace  User                  Auth  Ports  Session Duration
8f1c...     deployment/payments  payments   dev@corp.example.com  sso          5m
```

Sessions opened through a kubeconfig show `kube` there, so an admin can tell the two apart. The local `mirrord ui` daemon is the exception to the rule above: it still watches the Operator through your kubeconfig.

A config that names a kubeconfig or a context is still accepted: mirrord warns that the setting is ignored while you are logged in and runs against the Operator. A config with `operator: false` cannot run, because without the Operator there is nothing to log in to:

```text
kube_context is ignored while logged in to mirrord.corp.example.com
```

```text
operator: false can't be used while logged in to mirrord.corp.example.com. Run mirrord logout --url https://mirrord.corp.example.com first.
```

A [MetalBear Cloud](beyond-kubernetes/metalbear-cloud.md) config selects Cloud with that same `operator: false`, so log out of the Operator before running one. With several Operator logins saved, logging out of one switches to the next, so log out of each until `mirrord whoami` shows no Operator login.

Profiles work as before: the Operator reads the `MirrordClusterProfile` or `MirrordProfile` your config names on your behalf, because only the Operator's own API is reachable through its URL. That needs an Operator of version `3.239.0` or later; with an older one, a profile fails with ``Feature `mirrord profiles served by the operator API` is not supported in mirrord operator <version>.``

If the Operator's license drops below Enterprise while you are logged in, the next command fails with the same message every paid feature gives, naming SSO and the Enterprise plan. Log out, or wait for the admin to restore the license.

Because only the Operator's API is reachable, mirrord does not read the target workload itself: the Operator resolves it, the way it does for a multi-cluster session. Two things follow from that. The `auto_mount` option is skipped with a warning, since it reads the pod spec locally, and Kafka branching is not available while logged in. A config without a namespace runs in the `default` namespace, not in the kubeconfig's.

A running session keeps its connection when the login expires, but it cannot reconnect after a network cut once the login has expired: it stops with the expiry message, and you log in again before the next run.

`mirrord db-branches status`, `mirrord db-branches destroy` and `mirrord vpn` are not served through the Operator's URL yet, whether or not you have a kubeconfig. While you are logged in, `status` and `destroy` end with `No active DB branch found` even when branches exist, because the branch resources are not reachable, and `vpn` fails with a not-found error. Log out of each Operator first, then run them with your kubeconfig. `mirrord db-branches connections` reads local files only and keeps working.

### Picking a target

`mirrord ls` and the target pickers of the IDE extensions work while you are logged in, with no kubeconfig on the machine. The Operator lists the targets of the namespace for you, already filtered to what RBAC lets your SSO user and groups list, so the pickers show the targets you may list. A target you may `get` but not `list` does not appear there; name it in the config and the session still runs.

```bash
mirrord ls -n payments
```

The namespace picker of the IDE asks the Operator which namespaces you may list targets in, and offers those: every namespace the Operator serves where RBAC grants you `list` on targets, nothing else. Without a namespace in the config or on the command line, the listing starts in the `default` namespace, like a session does. When RBAC does not let you list targets there, the picker shows no targets until you switch to one of the namespaces it offers.

The namespace list needs an Operator of version `3.239.0` or later. With an older one, mirrord falls back to the namespaces that hold targets you may list cluster-wide, which is the current namespace alone when your roles grant you namespaces only. The pickers never fail because of the namespace list: when the Operator refuses it, for example because your user's roles predate the chart version that grants `list` on `namespaces` in the Operator's API group, the current namespace is the only one offered and the targets of that namespace are still listed.

## Seeing your logins

```bash
mirrord whoami
```

```text
OPERATORS
ACTIVE  URL                               ACCOUNT               EXPIRES
*       mirrord.corp.example.com          dev@corp.example.com  in 6 hours
        staging.mirrord.corp.example.com  dev@corp.example.com  expired

mirrord Cloud: dev@corp.example.com, expires in 12 hours
```

The `*` marks the Operator mirrord uses. Without an Operator login in use, the first line says so and names the kubeconfig context mirrord falls back to:

```text
No operator login in use. mirrord uses your kubeconfig context staging-eu.

mirrord Cloud: dev@corp.example.com, expires in 12 hours
```

## When a login expires

A login is valid for 24 hours from the moment you logged in, whatever your identity provider's own session length. A command that needs an expired login fails without opening a browser and tells you what to run:

```text
Your login to mirrord.corp.example.com expired. Run mirrord login --url https://mirrord.corp.example.com.
```

## Logging out

```bash
mirrord logout --url https://mirrord.corp.example.com
```

```text
Logged out of mirrord.corp.example.com. mirrord uses your kubeconfig context staging-eu.
```

Logging out removes that login from your machine. If it was the one in use, mirrord switches to the login you made most recently that has not expired, and to your kubeconfig when there is none. The same command with the Cloud URL logs you out of mirrord Cloud.

## Where logins are kept

Logins are kept in `~/.mirrord/auth.json`, readable by your user only. Each entry holds the Operator URL, your account, a token the Operator issued and the time it expires. The token is useless to another Operator. The Operator's admin can end every login at once by rotating the client secret of the mirrord application at the identity provider, updating the Kubernetes Secret that holds it and restarting every Operator replica: the Operator derives the key it signs tokens with from the secret at startup, so a replica still running on the old secret keeps accepting the old tokens. After that, a command fails with the message in the table below, and `mirrord logout --url` then `mirrord login --url` gets you a new login.

## Troubleshooting

| Message | Why | What to do |
| --- | --- | --- |
| `the operator at ... does not support mirrord login yet` | The Operator predates the login flow. | Ask your admin to upgrade the Operator to `3.239.0` or later. |
| `Feature sso requires a mirrord Enterprise plan.` | The Operator's license is Teams. | Ask your admin about the Enterprise plan. |
| `... is not a mirrord operator with SSO enabled, and it is not mirrord Cloud` | The URL answered, but not as an Operator with SSO or as mirrord Cloud does. | Check the URL with your admin: it is the chart's `sso.url`. |
| `could not reach ...` | The Operator's URL is not reachable from your machine. | Check your network, VPN and the URL. |
| `the login was not approved in the browser within 5 minutes` | The browser never came back to mirrord. | Run `mirrord login --url` again and finish the sign-in. |
| The browser shows `mirrord could not complete the login` | The identity provider refused, or the login link was older than ten minutes. | Run `mirrord login --url` again. |
| `kube_context is ignored while logged in to ...` | Your config names a kubeconfig or a context, and a login is in use. | Nothing, mirrord runs against the Operator. Log out of each Operator to use the kubeconfig again. |
| `operator: false can't be used while logged in to ...` | Your config turns the Operator off while a login is in use. | Run `mirrord logout --url` with that URL, or drop `operator: false`. |
| `Feature sso requires a mirrord Enterprise plan.` while running | The Operator's license dropped below Enterprise after you logged in. | Ask your admin about the license, or log out and use your kubeconfig. |
| ``Feature `mirrord profiles served by the operator API` is not supported in mirrord operator ...`` | Your config names a profile, and the Operator predates profile reads over its URL. | Ask your admin to upgrade the Operator to `3.239.0` or later, or drop the profile. |
| The IDE's namespace picker offers only the current namespace | Your roles predate the chart version that grants `list` on `namespaces` in the Operator's API group. | Ask your admin to upgrade the chart release to `3.239.0` or later. Until then, set the namespace in the config or with `mirrord ls -n`. |
| `the login token was not issued by this operator or has expired` before the time `mirrord whoami` shows | The Operator stopped accepting your token, for example after its client secret was rotated. `mirrord login --url` alone reuses the saved token while it has not expired. | Run `mirrord logout --url <operator-url>`, then `mirrord login --url <operator-url>`. |
| A command fails with `403` after a login | You are logged in, but RBAC grants your user or groups nothing in that namespace. | Ask your admin to bind your group, see [Granting a group access](../managing-mirrord/sso.md#granting-a-group-access-to-a-namespace). |
