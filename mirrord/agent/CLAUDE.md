## Quick Reference

```bash
cargo check -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going
```

## Architecture

### Targeted vs Targetless Mode

The binary is Linux-only. `entrypoint::main` picks one of two modes:

1. `start_agent(args)`: the actual agent logic (listens for clients, runs background tasks). Called directly in targetless mode, or as the child process in targeted mode.
2. `start_iptable_guard(args)`: targeted mode parent process. Spawns the child that runs `start_agent`, then waits and cleans up iptables on exit.

In targeted mode, there's always a parent/child split:
1. Parent spawns child with `MIRRORD_AGENT_CHILD_PROCESS=true`.
2. Child runs the normal agent logic.
3. Parent waits for the child to exit (or for SIGTERM), then cleans up iptables chains.

Before serving clients, the agent checks for leftover iptables rules. If rules already exist, it either fails fast (`AgentError::IPTablesDirty`) or cleans them up when `clean_iptables_on_start` is enabled.

### How the Runtimes and Namespaces Work

- The main agent loop runs on a single-threaded tokio runtime (`#[tokio::main(flavor = "current_thread")]`).
- Long-running network tasks run on a separate `BgTaskRuntime` thread.
- `BgTaskRuntime` may enter the target pod's network namespace (`setns`) before creating its tokio runtime.
- In targeted non-ephemeral mode, the network runtime runs inside the target's network namespace.
- In targetless and ephemeral mode, it stays in the current namespace.
- This split exists so agent shutdown stays clean even if a background task hangs.

## Things to Watch Out For

- Both main and background runtimes are single-threaded, so avoid blocking operations or busy loops on them.
- Don't log noisy warn/error messages for client-level failures; see @CONTRIBUTING.md for more guidance.
