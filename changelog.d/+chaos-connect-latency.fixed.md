Chaos latency rules no longer delay the response to a connect request. Holding
that response back blocked the calling thread, which stopped client-side timeouts
from firing during the fault. Latency now affects reads and writes only, so your
app can react to the delay. A `read_ms` value also costs its delay once per
operation, rather than twice on a new connection.
