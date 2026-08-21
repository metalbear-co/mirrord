Chaos latency rules no longer hold back the response to a connect request.
Holding it back blocked the calling thread, which in a single-threaded runtime
stalled the event loop, so a client-side timeout could not fire during the
fault it was written for. A connection now waits for its delay before it
carries data, on whichever of the first read or write comes first, which leaves
the application free to act on it. The total delay a request sees is unchanged.
