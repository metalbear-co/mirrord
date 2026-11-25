# mirrord-replay

Send an HTTP request that a mirrord session already received to the local application again with the `mirrord replay` CLI, instead of triggering it through the cluster again.

## What it does

This skill helps AI agents:
- **List** the requests a running session received, and read their headers and bodies
- **Replay** one of them to the local application, optionally with changed headers
- **Check a code change** by replaying the same request and comparing the responses
- **Explain** what is kept, what can't be replayed, and that a replay has real side effects

## Example prompts

```
"Replay the request my service just got"

"Resend that failing request to my local app"

"The upstream call times out while I sit on a breakpoint"

"How do I use mirrord replay?"
```

## Key commands

```bash
mirrord replay list
mirrord replay show 3
mirrord replay send 3
mirrord replay send 3 --header 'x-tenant: blue' --format json
```
