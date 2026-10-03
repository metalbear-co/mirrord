Session analytics now report a non-reversible hash of the connected cluster's identity, so
usage can be attributed to a cluster rather than inferred from network addresses. The hash is
derived from the `kube-system` namespace UID, a randomly generated value that describes nothing
about the cluster, and it is omitted when the identity cannot be read. Covered by the existing
telemetry opt-out.
