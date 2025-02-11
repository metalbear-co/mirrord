# Cluster-side configuration

Filtered HTTPS stealing is be configured using two resources: `MirrordTlsStealConfig` and `MirrordClusterTlsStealConfig`.
The only difference between the two is that `MirrordTlsStealConfig` can apply only to session targets living in the same namespace,
while `MirrordClusterTlsStealConfig` can apply to all session targets in the cluster.

An example of `MirrordTlsStealConfig`:

```yaml
apiVersion: mirrord.metalbear.co/v1alpha
kind: MirrordTlsStealConfig
metadata:
  # Name of the config resource is not inspected by the mirrord Operator,
  # and can be set to any value.
  name: sample-1
  # Config resource must be created in the same namespace as the mirrord session target.
  namespace: namespace-of-example-deployment
spec:
  # An optional wildcard pattern that will be matched against mirrord session target path.
  #
  # `*` character matches any sequence of characters
  # `?` character matches exactly one character
  #
  # mirrord session target path is built from the following components, separated with slashes:
  # 1. Target type: `deployment`, `pod`, `rollout`, ...
  # 2. Kubernetes name of the target resource, e.g `example-deployment`
  # 3. Optional `container` separator
  # 4. Optional target container name, e.g `example-container`
  #
  # For example:
  # 1. mirrord session against `example-container` container
  #    defined in a Deployment named `example-deployment`
  #    will use target path `deployment/example-deployment/container/example-container`.
  # 2. mirrord session against an unspecified container (picked by mirrord when the session starts)
  #    in a Pod named `example-deployment-aabbcc`
  #    will use target path `pod/example-deployment-aabbcc`.
  #
  # Pattern defined below will match both.
  targetPath: "*/example-deployment*"
  # An optional label selector that will be matched against mirrord session target labels.
  #
  # Selector defined below will match any target labelled with `my-label: my-value`.
  selector:
    matchLabels:
      my-label: my-value
  # A list of steal TLS configurations for distinct ports in the target container.
  # You can configure each port independently.
  ports:
    # This entry configures how mirrord-agent handles filtered stealing from port 443.
    - port: 443
      # This field configures how mirrord-agent authenticates itself when accepting stolen TLS connections.
      remoteServerAuth:
        # Path to a PEM file containing a certificate chain to use.
        #
        # This file must contain at least one certificate.
        # It can contain entries of other types, e.g private keys, which will be ignored.
        certPath: /path/to/server/cert.pem
        # Path to a PEM file containing a private key for the `certPath` certificate.
        #
        # This file must contain exactly one private key.
        # It can contain entries of other types, e.g certificates, which will be ignored.
        keyPath: /path/to/server/key.pem
        # Supported ALPN protocols.
        #
        # Optional, defaults to an empty list.
        alpnProtocols:
          - h2
          - http/1.1
      # This field configures how mirrord-agent authenticates peers when accepting stolen TLS connections.
      #
      # Optional. If not present, mirrord-agent's TLS server will not offer client authentication.
      remoteClientAuth:
        # Whether the sever should accept anonymous clients.
        allowUnauthenticated: true
        # A list of paths to PEM files containing allowed root certificates.
        #
        # Each certificate found in these files is treated as an allowed root.
        # These files can contain entries of other types, e.g private keys, which will be ignored.
        #
        # Invalid certificates will be ignored.
        rootCerts:
          - /path/to/client/root-1.pem
          - /path/to/client/root-2.pem
      # This field configures how mirrord-agent authenticates itself when making TLS connections
      # to the original destination (the TLS server running in the target container).
      #
      # mirrord-agent will make TLS connections to the original destination
      # when passing through requests that do not match any client filter.
      #
      # Optional. If not present, mirrord-agent's TLS client will be anonymous.
      agentClientAuth:
        # Path to a PEM file containing a certificate chain to use.
        #
        # This file must contain at least one certificate.
        # It can contain entries of other types, e.g private keys, which will be ignored.
        certPath: /path/to/client/cert.pem
        # Path to a PEM file containing a private key for the `certPath` certificate.
        #
        # This file must contain exactly one private key.
        # It can contain entries of other types, e.g certificates, which will be ignored.
        keyPath: /path/to/client/key.pem
```

Note: each path defined in a config resource here must be absolute and will be resolved in the target container.

# User-side configuration

In their personal mirrord configs, users will be able to configure how to deliver stolen HTTPS requests to their local applications:

1. Plain HTTP.
2. HTTPS with an anonymous client.
3. HTTPS with a client using a selected local certificate.
4. HTTPS with a client using the agent's client certifiacate (requires `agentClientAuth` to be filled in the config resource).
