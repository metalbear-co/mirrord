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
  # Specify the targets for which this configuration applies, in the `pod/my-pod`,
  # `deploy/my-deploy/container/my-container` notation.
  #
  # Targets can be matched using `*` and `?` where `?` matches exactly one
  # occurrence of any character and `*` matches arbitrary many (including zero) occurrences
  # of any character. If not specified, this configuration does not depend on the target's path.
  targetPath: "*/example-deployment*"
  # If this selector is provided, this configuration will only apply to targets with labels that match all
  # of the selector's rules.
  selector:
    matchLabels:
      my-label: my-value
  # Configuration for stealing TLS traffic, separate for each port.
  ports:
    # This entry configures how mirrord-agent handles filtered stealing from port 443.
    - port: 443
      # Configures how mirrord-agent authenticates itself and the clients when acting as a TLS
      # server.
      #
      # mirrord-agent acts as a TLS server when accepting stolen connections.
      agentAsServer:
        # Configures how the server authenticates itself to the clients.
        authentication:
          # Path to a PEM file containing a certificate chain to use.
          #
          # This file must contain at least one certificate.
          # It can contain entries of other types, e.g private keys, which are ignored.
          certPem: /path/to/cert.pem
          # Path to a PEM file containing a private key matching [`CertificateChainAuthentication::cert_pem`].
          # 
          # This file must contain exactly one private key.
          # It can contain entries of other types, e.g certificates, which are ignored.
          keyPem: /path/to/key.pem
        # ALPN protocols supported by the server, in order of preference.
        #
        # If empty, ALPN is disabled.
        #
        # Optional. Defaults to an empty list.
        alpnProtocols:
          - h2
          - http/1.1
          - http/1.0
        # Configures how mirrord-agent's server authenticates the clients.
        #
        # Optional. If not present, the server will not offer client authentication.
        verification:
          # Whether anonymous clients should be accepted.
          # 
          # Optional. Defaults to `false`.
          allowAnonymous: false
          # Whether to accept any certificate, regardless of its validity and who signed it.
          # 
          # Optional. Defaults to `false`.
          acceptAnyCert: false
          # Paths to PEM files and directories with PEM files containing allowed root certificates.
          # 
          # Directories are not traversed recursively.
          # 
          # Each certificate found in the files is treated as an allowed root.
          # The files can contain entries of other types, e.g private keys, which are ignored.
          # 
          # Optional. Defaults to an empty list.
          trustRoots:
            - /path/to/cert.pem
            - /path/to/directory/with/certs
      # Configures how mirrord-agent authenticates itself and the server when acting as a TLS
      # client.
      #
      # mirrord-agent acts as a TLS client when passing unmatched requests to their original
      # destination.
      agentAsClient:
        # Configures how mirrord-agent authenticates itself to the original destination server.
        #
        # Optional. If not present, mirrord-agent will make connections anonymously.
        authentication:
          # Path to a PEM file containing a certificate chain to use.
          #
          # This file must contain at least one certificate.
          # It can contain entries of other types, e.g private keys, which are ignored.
          certPem: /path/to/cert.pem
          # Path to a PEM file containing a private key matching [`CertificateChainAuthentication::cert_pem`].
          # 
          # This file must contain exactly one private key.
          # It can contain entries of other types, e.g certificates, which are ignored.
          keyPem: /path/to/key.pem
        # Configures how mirrord-agent verifies the server's certificate.
        verification:
          # Whether to accept any certificate, regardless of its validity and who signed it.
          acceptAnyCert: bool,
          # Paths to PEM files and directories with PEM files containing allowed root certificates.
          #
          # Directories are not traversed recursively.
          #
          # Each certificate found in the files is treated as an allowed root.
          # The files can contain entries of other types, e.g private keys, which are ignored.
          trustRoots:
            - /path/to/cert.pem
            - /path/to/directory/with/certs
```

```yaml
apiVersion: mirrord.metalbear.co/v1alpha
kind: MirrordTlsStealConfig
metadata:
  name: example
  namespace: namespace-of-example-deployment
spec:
  targetPath: "*/example-deployment*"
  ports:
    - port: 443
      agentAsServer:
        authentication:
          certPem: /path/to/cert.pem
          keyPem: /path/to/key.pem
        alpnProtocols:
          - h2
          - http/1.1
          - http/1.0
      agentAsClient:
        authentication:
          certPem: /path/to/cert.pem
          keyPem: /path/to/key.pem
        verification:
          trustRoots:
            - /path/to/root/cert.pem
            - /path/to/directory/with/root/certs
```
