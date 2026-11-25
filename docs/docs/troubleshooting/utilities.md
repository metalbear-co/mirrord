---
title: Utilities
date: 2026-02-02T12:59:39.000Z
description: Lightweight options that provide additional insight when debugging mirrord
---

# Troubleshooting Utilities

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

## Session Timings
The `mirrord diagnose timings` command shows where the time of your latest session went. This is useful when a session takes long to start, or when your application starts or responds slower with mirrord than without it, and you want to know what the time goes to.

The output has two sections:

- **Session start** is what the operator did before your application could start, phase by phase. It is shown for sessions that use an operator which records it.
- **Application calls** is what your application then waited on mirrord for. While your application runs, mirrord counts every call it answers from the cluster: name lookups, file operations, outgoing connections. Calls that stay on your machine are not counted, and neither is your application's own code.

Nothing has to be turned on. The calls are counted where mirrord intercepts them, so the language of your application does not matter.

```bash
mirrord diagnose timings [OPTIONS]
```
Options

`--session <SESSION>`: Show this session instead of the latest one of this machine. The id is what `mirrord exec` prints as "session ID".

`-f, --config-file <CONFIG_FILE>`: Config file to use for reaching the operator. Without it, the session's own cluster is asked.

`--json`: Print one JSON object with a key per section (`session_start`, `application`).

`-h, --help`: Print help information.

#### Example
```bash
mirrord diagnose timings
```

```bash
Session:  5E1D5B32275A0332 (deployment/sqs-consumer, mirrord 3.291.1)
Started:  2026-11-25T01:11:54Z

Session start
What the operator did before your application could start. A session connects more than once while it starts, and connect 1 is the first.

+---------+---------------------+----------------------------------------+----------+
| Connect | Phase               | What it is                             | Duration |
+---------+---------------------+----------------------------------------+----------+
| 1       | target_lookup       | finding the target                     |   1.29 s |
| 1       | config_and_policies | reading the configuration and policies | 686.0 ms |
| 1       | session_acquire     | registering the session                | 977.0 ms |
| 1       | queue_splits        | waiting for the queue split            |  72.82 s |
| 1       | agent_create        | creating the agent                     | 705.0 ms |
| 1       | agent_wait_ready    | waiting for the agent to be ready      |   1.45 s |
| 1       | agent_connect       | connecting to the agent                | 177.0 ms |
| all     |                     |                                        |  78.11 s |
+---------+---------------------+----------------------------------------+----------+

Application calls
What your application waited on mirrord for. Updated 2026-11-25T01:13:40Z (1s ago).

+------------------+-------+----------+----------+
| Kind             | Calls |    Total |  Slowest |
+------------------+-------+----------+----------+
| file stat        |    44 |   7.65 s | 206.0 ms |
| name lookup      |     3 | 693.7 ms | 299.6 ms |
| file read        |     2 | 366.0 ms | 203.1 ms |
| file open        |     1 | 200.4 ms | 200.4 ms |
| outgoing connect |     1 |   1.5 ms |   1.5 ms |
| all              |    51 |   8.91 s | 299.6 ms |
+------------------+-------+----------+----------+

Slowest calls:
 299.6 ms  name lookup  redis.queue-splitting.svc.cluster.local
 230.6 ms  name lookup  redis.queue-splitting.svc.cluster.local
 206.0 ms  file stat    /srv/demo-app/plugins/plugin-22.yaml

Suggestions:
- 40 file lookups under `/srv/demo-app/plugins` went to the cluster (about 6.94 s) and every one came back "not found". Add "^/srv/demo-app/plugins/" to `feature.fs.not_found` to give the same answer without the round trip, or to `feature.fs.local` if those files are on your machine.
```

**Phase:** the operator's own name for a step, with a few words on what it is. A phase the session did not need is not listed.

**Calls:** how many calls of that kind mirrord answered from the cluster.

**Total / Slowest:** how long your application waited for those answers, all together and for the single longest call. Time spent sending and receiving data over connections is not included.

**Slowest calls:** the slowest single calls, each at least a tenth of a second, with what they were for: the host of a name lookup, the path of a file operation, the address of a connection.

**Suggestions:** printed only when the numbers alone settle what to change, so most sessions show none.

The application's numbers are kept in `~/.mirrord/timings`, one file per session, and never leave your machine. The 10 newest are kept, and older ones are removed once they are a day old. They follow the application within about a second, and a process that is killed keeps its calls in the summary. Sessions started with `mirrord container` leave no file. To turn the counting off, set [`experimental.call_timings`](https://metalbear.com/mirrord/docs/config#experimental-call_timings) to `false`.

The operator keeps the start of recent sessions in memory, so a session it no longer remembers, or an operator that does not record this yet, shows one line under "Session start" and the application's numbers below it.
