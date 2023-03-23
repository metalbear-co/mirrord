# Debugging mirrord

Debugging mirrord can get hard since we're running from another app flow, so the fact we're debugging might affect the program and make it unusable/buggy (due to sharing stdout with scripts/other applications).

The recommended way to do it is to use `mirrord-console`. It is a small application that receives log information from different mirrord instances and prints it, controlled via `RUST_LOG` environment variable.

To use mirrord console, run it:
`cargo run --bin mirrord-console`

Then run mirrord with the environment variable:
`MIRRORD_CONSOLE_ADDR=127.0.0.1:11233`

## Retrieving Agent Logs

By default, the agent's pod will complete and disappear shortly after the agent exits. In order to be able to retrieve 
the agent's logs after it crashes, set the agent's pod's TTL to a comfortable number of seconds. This configuration can
be specified either as a command line argument (`--agent-ttl`), environment variable (`MIRRORD_AGENT_TTL`), or in a
configuration file:
```toml
[agent]
ttl = 30
```

Then, when running with some reasonable ttl, you can retrieve the agent log like this:
```bash
kubectl logs -l app=mirrord --tail=-1 | less -R
```

This will retrieve the logs from all running mirrord agents, so it is only useful when just one agent pod exists.

If there are currently multiple agent pods running on your cluster, you would have to run
```bash
kubectl get pods
```
and find the name of the agent pod you're interested in, then run

```bash
kubectl logs <YOUR_POD_NAME> | less -R
```

where you would replace `<YOUR_POD_NAME>` with the name of the pod.
