# mirrord-propagation-discovery

Find out what mirrord can filter traffic on in **your** cluster: scan each service's live incoming traffic with `mirrord dump`, check which identifier (a `baggage` member, a correlation-id header, a tenant or user id, a trace id, a JWT claim, a cookie, a path segment, a body field) is present on every hop, unique to one developer, and settable by that developer, and turn it into a `mirrord.json` filter. No application code changes.

## What it does

This skill helps AI agents:
- **Install** the mirrord CLI through a package manager when it is missing (never a piped remote script)
- **Map** the hops of a request flow from the cluster: ingress, services, workloads, ports
- **Dump** every hop at once in mirror mode, send one tagged probe request at the edge (or observe real traffic), and read the output
- **Score** the identifiers found per hop with a stdlib helper: presence, unique-per-request vs stable, probe hits, values shared across hops; JWT claims and `baggage`/`tracestate` members are decoded, secrets are never printed
- **Build** the matching filter: `header_filter` regex, `header_filter_jq` for JWTs and cookies, `path_filter`, `body_filter`, or `all_of`/`any_of` combinations, with `{{ key }}` templating
- **Verify** it in a mirror session on the deepest hop before anyone steals, with a stdlib request recorder as the local process so the copied request causes no duplicate writes
- **Recognise** anything else that travels between hops, from a reference of vendor trace headers, legacy baggage names, auth-proxy identity headers, routing and feature-flag headers, and queue and event carriers
- **Report** what to filter on, which services it covers and which it does not (and why), which hops a dump can't observe (queue consumers, TLS terminated in the pod), a complete runnable `mirrord.json`, and what the `mirrord-header-propagation` skill has to fix. The report is printed and saved as `.mirrord/propagation-discovery.md`

## Example prompts

```
"Which header or token claim can I filter on so I only get my own requests across all our services?"

"Does our staging cluster already propagate a request id or trace context end to end?"

"Run mirrord dump on the gateway, orders and billing services and tell me what survives every hop"

"I want to filter on my user's JWT with mirrord — check that the token reaches the inventory service"

"Our x-request-id filter works on the API but not on the worker behind it — find something that does"

"Can I filter on my tenant id, or on the trace id our tracer propagates, across every hop?"
```

## Prerequisites

- `kubectl` access to a staging or development cluster
- mirrord CLI **3.191.0+** (`mirrord dump` port auto-detection); **3.194.0+** for jq header filters
- A way to send a request at the edge (ingress URL or `kubectl port-forward`) for the active probe; passive observation works without it
- Nothing Team/Enterprise-specific: `mirrord dump` and HTTP filters are open source. Filtering traffic that the pod itself decrypts needs the Operator's HTTPS stealing setup

## Learn more

- [Inspecting live traffic with `mirrord dump`](https://metalbear.com/mirrord/docs/using-mirrord/incoming-traffic/inspect-live-traffic)
- [Filtering incoming traffic](https://metalbear.com/mirrord/docs/using-mirrord/incoming-traffic/filter-incoming-traffic)
- [Stealing HTTPS traffic](https://metalbear.com/mirrord/docs/using-mirrord/incoming-traffic/steal-https)
