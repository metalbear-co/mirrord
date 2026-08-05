Updated the `kube` fork to 4.2.0. `TCP_NODELAY` is now set on connections to the Kubernetes API
server, so requests are not held back by Nagle's algorithm.
