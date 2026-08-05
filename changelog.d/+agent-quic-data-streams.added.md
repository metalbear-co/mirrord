Outgoing TCP connections handled for a session now travel on a QUIC stream of their own, rather than
being framed into protocol messages alongside everything else the session is doing. Each connection
gets its own ordering and its own flow control window, so a large or slow transfer no longer holds up
the rest of the session between the operator and the agent.
