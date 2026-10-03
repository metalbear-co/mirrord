Session analytics now report the identifier of the cluster a session connected to, so usage
can be attributed to a cluster rather than inferred from network addresses. The value is the
UID of the cluster's `default` namespace, the same per-cluster identifier the operator already
reports, and it is omitted when that identity cannot be read. Covered by the existing telemetry
opt-out.
