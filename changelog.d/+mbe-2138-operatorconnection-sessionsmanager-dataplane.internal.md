Decouple the operator WebSocket connection and upgrade plumbing from the full operator client
feature. The new `mirrord-operator` `connection` feature exposes a protocol-endpoint-generic
`OperatorConnection` and `connect_ws_direct`, allowing hyper clients to use either the Kubernetes
API-server or direct WebSocket upgrade contract. This prepares sessions-manager to reuse the
transport without depending on the full operator API client.
