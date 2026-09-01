Fixed a stolen HTTP/2 request being sent to the local application over a pooled HTTP/1 connection, which made the protocol the application saw depend on what was in the connection pool.
