---
title: Logging in with SSO
description: Log in to the mirrord Operator through your company's identity provider with mirrord login, see your logins with mirrord whoami, and remove one with mirrord logout.
tags:
  - enterprise
---

# Logging in with SSO

When your team's mirrord Operator has [single sign-on](../managing-mirrord/sso.md) turned on, you log in to it through your company's identity provider instead of a kubeconfig. One login lasts 24 hours, and mirrord uses it for every command that talks to that Operator until it expires or you log out.

You need mirrord `3.291.0` or later and an Operator of version `3.236.0` or later with an Enterprise license. Your admin gives you the Operator's URL, the `sso.url` value of its Helm chart.

## Logging in

```bash
mirrord login --url https://mirrord.corp.example.com
```

mirrord opens your browser at your identity provider. Once you have signed in there, the browser shows a page saying you are logged in and the command prints who you are logged in as and until when:

```text
Logged in to mirrord.corp.example.com as dev@corp.example.com. The login is valid until 2026-11-20 09:12. mirrord uses this operator now.
```

Before the browser opens, mirrord checks the URL. An Operator without SSO, or one older than `3.236.0`, is refused with a message naming what is missing. An Operator whose license does not include SSO is refused with the same error every paid feature gives, naming SSO and the Enterprise plan. A URL that is neither an Operator nor mirrord Cloud is refused too, so a typo never sends you to a login page it should not.

If your browser does not open, the command prints the link to open by hand. The login finishes when the browser reaches mirrord's page on your machine, so both must run on the same computer.

The same command logs you in to mirrord Cloud when the URL is the Cloud's, `https://app.metalbear.com` by default. A Cloud login lives next to your Operator logins and never changes which Operator mirrord uses.

### Several Operators

You can be logged in to several Operators at once, for example production and staging. mirrord uses the one you logged in to last. To switch to another one you are still logged in to, run `mirrord login --url` with its URL again: when that login has not expired, mirrord switches without opening a browser.

```text
Logged in to staging.mirrord.corp.example.com as dev@corp.example.com already, valid until 2026-11-20 09:12. mirrord uses this operator now.
```

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

Logins are kept in `~/.mirrord/auth.json`, readable by your user only. Each entry holds the Operator URL, your account, a token the Operator issued and the time it expires. The token is useless to another Operator, and the Operator's admin can end every login at once by rotating the client secret of the mirrord application at the identity provider.

## Troubleshooting

| Message | Why | What to do |
| --- | --- | --- |
| `the operator at ... does not support mirrord login yet` | The Operator predates the login flow. | Ask your admin to upgrade the Operator to `3.236.0` or later. |
| `Feature sso requires a mirrord Enterprise plan.` | The Operator's license is Teams. | Ask your admin about the Enterprise plan. |
| `... is not a mirrord operator with SSO enabled, and it is not mirrord Cloud` | The URL answered, but not as an Operator with SSO or as mirrord Cloud does. | Check the URL with your admin: it is the chart's `sso.url`. |
| `could not reach ...` | The Operator's URL is not reachable from your machine. | Check your network, VPN and the URL. |
| `the login was not approved in the browser within 5 minutes` | The browser never came back to mirrord. | Run `mirrord login --url` again and finish the sign-in. |
| The browser shows `mirrord could not complete the login` | The identity provider refused, or the login link was older than ten minutes. | Run `mirrord login --url` again. |
| A command fails with `403` after a login | You are logged in, but RBAC grants your user or groups nothing in that namespace. | Ask your admin to bind your group, see [Granting a group access](../managing-mirrord/sso.md#granting-a-group-access-to-a-namespace). |
