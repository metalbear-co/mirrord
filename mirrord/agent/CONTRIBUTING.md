# Tracing

Mind that in mfT:
1. mirrord-agent is a **shared** resource. It can be used by multiple mirrord sessions at the same time.
2. mirrord-agent logs are most likely collected and monitored. Any warnings or errors will alarm the person responsible for cluster management.
3. Remote IO requests originate from **untested code**.

Be very careful when logging on or above the default log level, which is `INFO`.
Such logs should be meaningful to the mirrord admin, not only to the user that runs the session.

Examples of mirrord-agent events that **are not** meaningful to the mirrord admin:

1. Failed to make an outgoing connection requested by a session
2. Failed to resolve DNS requested by a session
3. Failed to deliver a stolen HTTP request, because the session has disconnected

Examples of mirrord-agent events that **are** meaningful to the mirrord admin:

1. Failed in an unexpected way, i.e. we have a bug in the agent.
2. Encountered a fatal error, and most probably is not configured properly.
   E.g. failed to join target's network namespace, does not have permissions to read target's `/etc/resolv.conf`
3. Received a malformed mirrord-protocol message. This would most likely be a bug in the client code.
4. Failed to make a passthrough connection to the target.
   This might be on us or on the target application. Either way, interesting.
5. Encountered an error in an incoming HTTP connection.
   This might be on us or on the HTTP client application. Either way, interesting.

### `tracing::instrument`

When instrumenting a fallible function, mind that by default `err` uses `Level::ERROR` to log the error:

```rust
#[tracing::instrument(
    level = Level::TRACE,
    ret, // <- `tracing` will emit a log on the span level
    err, // <- without explicit level, `tracing` will emit a log on `Level::ERROR`
)]
async fn open_file(path: &Path) -> io::Result<File> {
    unimplemented!()
}
```

If this is not what you want, you can do this:

```rust
#[tracing::instrument(
    level = Level::TRACE,
    ret,
    err(level = Level::TRACE),
)]
async fn open_file(path: &Path) -> io::Result<File> {
    unimplemented!()
}
```

# Blocking Tokio's event loop

mirrord-agent uses one or two (depending on mode) **single-threaded** Tokio runtimes.
This means that doing any kind blocking IO will most probably make the agent temporarily unresponsive.
Do not run any blocking IO in the agent. If you have no natively async way to do your thing,
always use [blocking tasks](https://docs.rs/tokio/latest/tokio/task/#blocking-and-yielding).

### Busy loops

Even if your code does not do any blocking IO, you can still kill the runtime with a busy loop.
Like this:

```rust
struct HttpConnection {
    // ...
}

impl HttpConnection {
    /// Starts a graceful shutdown of this HTTP connection, allowing started requests to finish.
    /// After calling this, the connection should eventually stop yielding new requests.
    fn start_graceful_shutdown(&mut self) {
        // ...
    }

    async fn next_request(&mut self) -> Option<HttpRequest> {
        // ...
    }
}

async fn handle_single_request(request: HttpRequest) {
    // ...
}

async fn handle_requests(
    mut connection: HttpConnection,
    token: CancellationToken,
) {
    let mut requests = FuturesUnordered::new();

    loop {
        tokio::select! {
            biased;
            // Wrapping up, start graceful shutdown on the connection.
            // We don't want to receive any more requests.
            _ = token.cancelled() => {
                connection.start_graceful_shutdown();
            },
            // Progress previously received requests.
            Some(()) = requests.next() => {},
            // Receive the next request.
            request = connection.next_request() => {
                let Some(request) = request else {
                    // Connection is finished.
                    // Wait for requests to be processed, and exit.
                    requests.for_each(std::future::ready).await;
                    break;
                };
                requests.push(handle_single_request(request));
            },
        }
    }
}
```
Seemingly, `handle_requests` implementation is fine. There's no blocking IO anywhere.
However, from the moment the token is cancelled, the future returned from `token.cancelled()` will always be instantly ready.
Also, since there is no `.await` point in this branch, the control will never go back to the runtime, and no other future will ever be progressed.
The `tokio::select!` will always go into the first branch, and the whole agent will be stuck at calling `connection.start_graceful_shutdown()` in a loop,
doing **no other work**.

Even if there was an `.await` point in the first branch, and the `tokio::select!` was not `biased`,
polling `token.cancelled()` in a loop would still result burn the CPU, and dramatically reduce agent's responsiveness.

More examples of similar buggy loops:

1. Reading from a connection that already returned EOF.
2. Receiving from an `mpsc` channel that already returned `None`.
3. Polling an empty `FuturesUnordered`/`FuturesOrdered`/`JoinSet`.
