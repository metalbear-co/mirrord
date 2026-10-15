---
title: Utilities
date: 2026-02-02T12:59:39.000Z
description: Lightweight options that provide additional insight when debugging mirrord
---

## Response Header Injection
Indicate whether the request was handled by mirrord.

{% hint style="info" %}
This feature requires at least mirrord-agent version **3.163.0**.
{% endhint %}

### Use Case
When debugging, it’s often useful to know how a response was handled by mirrord after being intercepted.
This option helps verify mirrord’s routing decisions and understand whether the intercepted traffic was forwarded to the local process or passed through to its original destination.

### Header behavior
When enabled, the mirrord agent automatically adds a `mirrord-agent` header to HTTP responses handled by mirrord.

#### Possible values for the header:
- `forwarded-to-client`: The mirrord agent intercepted the request, sent it to the local process, and then passed back the response (mirrord handled it).
- `passed-through`: The mirrord agent intercepted the request, sent it to its original destination, and then passed back the response (the request was not handled by the local process). For example, if a request doesn’t match active filters, it will be passed to the original destination.

Header injection is disabled by default.
You can enable it with the following configuration in the `mirrord.json` file:

```json
{
  "agent": {
    "inject_headers": true
  }
}
```

You can see all the agent configuration options [here](https://metalbear.com/mirrord/docs/config#agent).

## Latency Diagnose
The `mirrord diagnose latency` command helps identify network latency issues between your local environment and the target workload. It measures round-trip time (RTT) across multiple iterations and reports latency statistics, making it useful when mirrord feels slow or requests take longer than expected.

### When to use this
Use this command if you experience:

- Slow request/response times when using mirrord
- Unexpected delays compared to running locally or in-cluster
- Network issues between your machine and the Kubernetes cluster

```bash
mirrord diagnose latency [OPTIONS]
```
Options

`-f, --config-file <CONFIG_FILE>`: Specify a config file to use.

`-h, --help`: Print help information.

#### Example
```bash
mirrord diagnose latency
```

```bash
...
* 89/100 iterations completed, last iteration took 397ms
...
✓ Latency statistics: min=40ms, max=397ms, avg=47ms
  ✓ using operator
    ✓ operator license valid
    ✓ user credentials prepared
    ✓ session started
      ✓ connected to the target

```
**Iterations:** mirrord performs multiple RTT checks to smooth out transient spikes.

**min / max / avg:**
- `min` represents best-case latency
- `max` highlights potential spikes or instability
- `avg` is usually the most useful indicator for overall performance

