The session connection to the operator can now be made directly over QUIC, instead of being proxied
by the Kubernetes API server, when the operator installation exposes an endpoint for it. Sessions
fall back to the API server whenever the direct connection is unavailable, and
`direct_operator_connection` turns it off.
