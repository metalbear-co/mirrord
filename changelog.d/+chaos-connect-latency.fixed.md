Chaos latency rules no longer delay the response to a connect request. Applying a
latency effect previously held that response back, which blocked the calling
thread while the connection was set up and stopped client-side timeouts in
single-threaded runtimes from firing during the fault. Latency is now applied to
reads and writes only, so it no longer prevents an application from reacting to
the delay it is being asked to survive. As a side effect, a `read_ms` value is no
longer charged a second time when a connection is opened.
