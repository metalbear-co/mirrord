## Runtimes and Network Namespaces

The main agent loop runs on a single-threaded tokio runtime (`#[tokio::main(flavor = "current_thread")]`), while
long-running network tasks run on a separate `BgTaskRuntime` thread. Before creating its tokio runtime, `BgTaskRuntime`
may enter the target pod's network namespace (`setns`): in targeted non-ephemeral mode, the network runtime runs inside
the target's network namespace, while in targetless and ephemeral mode it stays in the current namespace. This split
exists so agent shutdown stays clean even if a background task hangs.

## Rules

- Both main and background runtimes are single-threaded, so avoid blocking operations or busy loops on them.
- Don't log noisy warn/error messages for client-level failures; see @CONTRIBUTING.md for more guidance.
