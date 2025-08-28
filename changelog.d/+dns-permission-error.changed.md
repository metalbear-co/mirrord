A mirrord session can now be terminated early if a remote DNS lookup fails with `permission denied` error.
This error indicates that the Kubernetes cluster is hardened and the mirrord-agent might not be fully functional.
The behavior is controlled with the `experimental.dns_permission_error_fatal` setting, and enabled by default in OSS.