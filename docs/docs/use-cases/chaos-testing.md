---
title: Chaos Testing
date: 2026-07-15T21:00:00.000Z
lastmod: 2026-07-15T21:00:00.000Z
draft: false
menu:
  docs:
    parent: using-mirrord
toc: true
description: Introduce unexpected failures or disruptions to chaos test your app with mirrord.
tags:
  - alpha
  - oss
  - team
  - enterprise
---

# Chaos Testing

mirrord lets you inject artificial failures and disruptions into your app's outgoing traffic, so you can test how it behaves under unexpected conditions. Create rules that add latency to connections to specific hosts, or turn them into errors, and scope each rule to a single mirrord session.

## How chaos rules work

A chaos rule pairs a selector, which picks the traffic to disrupt, with an effect, which defines the disruption. Rules are attached to a single mirrord session and never affect other sessions.

### Selectors

A selector matches outgoing connections by their destination:

- `upstream`: the destination host, or `host:port` to match a specific port.
- `percentage`: roughly how often a matched connection gets the effect (0 to 100).

{% hint style="info" %}
Currently selectors can only match outgoing TCP connections. Selectors for file operations and HTTP requests are planned.
{% endhint %}

### Effects

An effect defines what happens to a matched connection. Two effects are supported:

- `latency`: delays the connection's read and/or write operations.

```json
{
  "name": "latency for api.example.com",
  "priority": 0,
  "selector": {
    "upstream": "api.example.com:443",
    "percentage": 100
  },
  "effect": {
    "latency": {
      "read_ms": 100,
      "write_ms": 200,
      "jitter_ms": 25
    }
  }
}
```

- `connection_error`: fails the connection. `type` can be one of: `reset` (can be applied to ongoing connections), `timed_out`, `refused`.

```json
{
  "name": "error for api.example.com",
  "priority": 0,
  "selector": {
    "upstream": "api.example.com:443",
    "percentage": 100
  },
  "effect": {
    "connection_error": {
      "type": "reset",
      "after_ms": 0
    }
  }
}
```

### Priority

When multiple rules match the same connection, only one is applied: the rule with the highest `priority` value. If not set, `priority` defaults to 0, the lowest.

## Managing chaos rules

### Option 1: CLI

**Prerequisites:** Minimum mirrord CLI version `3.241.0`

The easiest way to manage chaos rules is with the `mirrord chaos` commands. To see available commands and options:

```bash
mirrord chaos --help
```

All you need to manage rules for a mirrord session is the session ID. When starting a session by running `mirrord exec`, for example, this is printed by default:

```sh
* session ID: c425f391-e9cc-4199-8de9-7bdbb3e7dfcc
* Running command: ...
```

Alternatively, you can run `mirrord sessions` to show the details of running sessions.

For convenience, you can export the value:

```sh
export SESSION_ID='c425f391-e9cc-4199-8de9-7bdbb3e7dfcc'
```

#### Creating a rule

To add a new rule, for example:

```json
{
  "name": "latency for database interactions",
  "priority": 10,
  "effect": {
    "latency": {
      "read_ms": 750
    }
  },
  "selector": {
    "upstream": "sonic.database.svc.cluster.local",
    "percentage": 35
  }
}
```

Either write the rule to a file `path/to/rule.json` and run:

```sh
mirrord chaos add -s $SESSION_ID -f path/to/rule.json
```

Or, add a new rule straight from `stdin`:

```sh
# any command that prints to stdout can be used
cat path/to/rule.json | mirrord chaos add -s $SESSION_ID
```

{% hint style="info" %}
Multiple rules can be added at once by providing an array of rules instead of just one. Note that the rules are still added individually internally, so if one of the rules is invalid, the others may still be added.
{% endhint %}

#### Listing rules

To show the rules for the session:

```sh
mirrord chaos list -s $SESSION_ID
```

#### Modifying a rule

To edit an existing rule, the rule ID is required - this is printed when a rule is added with `mirrord chaos add`, or can be found in `mirrord chaos list`:

```sh
export RULE_ID='6b8f1c4e-2a73-4d9b-8e56-c3f0a7d1b924'

mirrord chaos edit -s $SESSION_ID -r $RULE_ID -f path/to/new_rule.json
```

As with `mirrord chaos add`, new rules can be provided from a file or `stdin`. Only one rule can be modified at a time.

#### Deleting a rule

To remove a specific rule:

```sh
mirrord chaos delete -s $SESSION_ID -r $RULE_ID
```

To remove all rules for this session:

```sh
mirrord chaos delete -s $SESSION_ID
```

#### Formatting

By providing the `--format` argument, the output of `mirrord chaos` can be customised:

- `pretty` (default): print a human readable summary
- `json`: print output in JSON
- `silent`: do not print output (errors are still printed to `stderr`)

When using `mirrord chaos` in scripts, it is recommended that you use `--format json` as the default `pretty` output may be changed in the future.

### Option 2: UI dashboard

{% hint style="info" %}
For more information on the UI server, see [Local UI](../using-mirrord/local-ui.md).
{% endhint %}

**Prerequisites:** Minimum mirrord CLI version `3.235.0`

In addition to the `chaos` sub-commands, it is possible to manage and view chaos rules in mirrord's UI server. When running the `mirrord chaos` command, it starts the UI silently in the background if it is not already running. To start the server and open the dashboard in your browser, run:

```sh
mirrord ui
```

The `chaos` tab on the right hand pane allows you to manage chaos rules interactively.

![UI Server Chaos Tab](../.gitbook/assets/ui-server-chaos-tab.png)

You can also see traffic for the session, and add rules from here.

![UI Server Traffic Logs](../.gitbook/assets/ui-server-traffic-hover.png)

#### Stopping the server

To stop the UI server manually, for example if it is misbehaving, run:

```sh
mirrord ui stop
```

### Option 3: REST endpoints

**Prerequisites:** Minimum mirrord CLI version `3.232.0`

#### Setup: UI server and starting a session

Chaos rules are managed through the mirrord UI server, so make sure it is running:

```sh
mirrord ui
```

{% hint style="info" %}
For more information on the UI server, see [Local UI](../using-mirrord/local-ui.md).
{% endhint %}

Grab the `API token` and the server address from the output:

```sh
* Web UI:
 -> http://127.0.0.1:59281/auth?token=60733e94f1799442e2f5af9048fac1a0efd6051715a2131992784f9c11876349&redirect=/
* API token:
 -> x-auth-token: 60733e94f1799442e2f5af9048fac1a0efd6051715a2131992784f9c11876349
```

Next, start the mirrord session you want to apply chaos rules to. In this example we run it from the terminal, but you can start it from the IDE plugins too:

```sh
mirrord exec -f .mirrord/mirrord.json -- node app.js
```

Grab the `session ID` from the session output (logs shortened for clarity):

```sh
* session ID: c425f391-e9cc-4199-8de9-7bdbb3e7dfcc
* Running command: node app.js
```

The chaos API needs three values: the server address, the `API token` that authenticates each request, and the `session ID` that scopes your rules to this session only. Export them once with your own values, and every example below can be copied as-is:

```sh
export UI_ADDRESS='http://127.0.0.1:59281'
export CHAOS_TOKEN='60733e94f1799442e2f5af9048fac1a0efd6051715a2131992784f9c11876349'
export SESSION_ID='c425f391-e9cc-4199-8de9-7bdbb3e7dfcc'
export CHAOS_URL="$UI_ADDRESS/api/chaos/rules/$SESSION_ID"
```

#### Managing rules via the API

##### Creating a rule

To create a chaos rule, write it to a JSON file, for example `latency-rule.json`:

```json
{
  "name": "latency for database interactions",
  "priority": 10,
  "effect": {
    "latency": {
      "read_ms": 750
    }
  },
  "selector": {
    "upstream": "sonic.database.svc.cluster.local",
    "percentage": 35
  }
}
```

Then send it in an HTTP POST request to the UI server with the `x-auth-token` header:

```sh
curl --request POST \
  --header 'Content-Type: application/json' \
  --header "x-auth-token: $CHAOS_TOKEN" \
  --data @latency-rule.json \
  "$CHAOS_URL"
```

The response returns the created rule, including the `id` you'll need to modify or delete it later. Note that in responses the `effect` is nested inside the `selector`, and `upstream` comes back with an explicit port, where `0` means any port:

```json
{
  "id": "6b8f1c4e-2a73-4d9b-8e56-c3f0a7d1b924",
  "name": "latency for database interactions",
  "priority": 10,
  "selector": {
    "type": "tcp",
    "upstream": "sonic.database.svc.cluster.local:0",
    "percentage": 35,
    "effect": {
      "latency": {
        "read_ms": 750
      }
    }
  },
  "hit_count": 0
}
```

The outgoing traffic (roughly 35% of it) from the service we are debugging with mirrord will now be affected by latency on read operations, when this traffic is destined to a host that matches the `selector`. In this example, outgoing traffic on this database connection would be impacted by this latency rule, so we can test how the `app.js` service behaves when there's latency requesting data from this database.

##### Listing rules

To list all the chaos rules that are active for a session, send an HTTP GET request:

```sh
curl --request GET \
  --header "x-auth-token: $CHAOS_TOKEN" \
  "$CHAOS_URL"
```

The response is an array of rules in the same shape as the creation response, each with its `id`.

##### Modifying a rule

Modifying or deleting a rule requires its ID, which is returned when the rule is created and included in the list response. To modify a rule, edit the rule's JSON file:

```json
{
  "name": "latency for database interactions more often",
  "priority": 10,
  "effect": {
    "latency": {
      "read_ms": 750
    }
  },
  "selector": {
    "upstream": "sonic.database.svc.cluster.local",
    "percentage": 75
  }
}
```

Then send it in an HTTP PUT request with the rule ID at the end of the URL:

```sh
export RULE_ID="6b8f1c4e-2a73-4d9b-8e56-c3f0a7d1b924"
curl --request PUT \
  --header 'Content-Type: application/json' \
  --header "x-auth-token: $CHAOS_TOKEN" \
  --data @latency-rule.json \
  "$CHAOS_URL/$RULE_ID"
```

Here we changed both the `name` of the chaos rule and its `percentage`.

##### Deleting a rule

To delete a chaos rule, send an HTTP DELETE request with the ID of the rule you want:

```sh
curl --request DELETE \
  --header "x-auth-token: $CHAOS_TOKEN" \
  "$CHAOS_URL/$RULE_ID"
```

The chaos rule (in this example `6b8f1c4e-2a73-4d9b-8e56-c3f0a7d1b924`) will be deleted, and will stop affecting the session immediately.

{% hint style="info" %}
Your AI assistant can generate and manage chaos rules for you. Install the [mirrord skills plugin](../using-mirrord-with-ai/ai-skills-plugin.md) and ask it, for example, to "add 750ms latency to my service's database connections".
{% endhint %}
