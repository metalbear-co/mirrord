---
title: Global Configuration
description: Set and remove user-wide mirrord CLI defaults
---

mirrord stores user-wide configuration in `~/.mirrord/mirrord.json`. Use it for a preference you want to keep across projects, rather than adding the same setting to every project's config file. The CLI creates the file as an empty JSON object when it is missing.

{% hint style="warning" %}
**Currently, only `operator` is used as a global default when starting a session.** The commands below can store other valid mirrord configuration fields, but those fields do not yet affect sessions through the global file. Set them in a [project configuration file](cli.md#configuration) instead.
{% endhint %}

## Set a default

Run `mirrord config set <PATH> <VALUE>` to save a value in the global file. For example, to require the mirrord Operator by default:

```bash
mirrord config set operator true
```

Use `false` to disable it by default:

```bash
mirrord config set operator false
```

If mirrord successfully starts a session using the Operator, it also remembers `operator: true` in this file for future sessions. An explicit project config value (including `false`) or the `MIRRORD_OPERATOR_ENABLE` environment variable takes precedence over the global default.

`PATH` is a dot-separated config field name, such as `operator` or `agent.image`. `VALUE` is parsed as JSON when possible (for example, `true`, `42`, or a JSON object); otherwise it is stored as a string. To force a string that looks like JSON, include JSON double quotes in the value, for example `mirrord config set agent.image '"true"'`. Fields and values must match the [mirrord configuration schema](https://metalbear.com/mirrord/docs/config). Setting a field within an array by index is not supported.

## Remove a default

Run `mirrord config unset <PATH>` to remove a value from the global file:

```bash
mirrord config unset operator
```

Removing `operator` restores the usual behavior: mirrord tries the Operator first and can continue without it if unavailable. Unsetting a field that is not present returns an error. Both commands reject invalid config changes rather than saving an invalid file.

You can inspect or edit `~/.mirrord/mirrord.json` directly as JSON. The `set` and `unset` commands require an existing file to be valid, untemplated JSON; a templated config file can be read during session setup but cannot be updated by these commands.
