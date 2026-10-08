---
title: Steal HTTPS Requests
date: 2025-02-24T00:00:00.000Z
lastmod: 2025-02-24T00:00:00.000Z
draft: false
menu:
  docs:
    parent: using-mirrord
weight: 166
toc: true
tags:
  - beta
  - team
  - enterprise
description: "Decrypt and steal HTTPS traffic using TLS certificates"
---

mirrord supports HTTPS stealing across all tiers.
However, when the deployed application receives encrypted traffic directly - for example, when HTTPS is not decrypted by the mesh or load balancer and the app itself handles TLS termination - this functionality is available only in mirrord for Teams.
With mirrord for Teams, you can steal a subset of HTTP requests coming to your target, even if the deployed application receives the traffic encrypted with TLS.

{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

**Important:** stealing HTTPS with a filter requires mirrord-operator version at least `3.106.0` and mirrord-agent version at least `1.134.0`. `agentAsClient.verification.serverName` requires mirrord-operator version at least `3.210.0` and mirrord-agent version at least `3.262.0`.

## Configuring HTTPS stealing in the cluster

To enable mirrord users to steal HTTPS requests with a filter, you must provide the mirrord Operator with some insight into your TLS configuration. This can be done with dedicated custom resources: `MirrordTlsStealConfig` and `MirrordClusterTlsStealConfig`. These two resources look and work almost the same. The only exception is that `MirrordTlsStealConfig` is scoped to the namespace in which you create it, while `MirrordClusterTlsStealConfig` scopes the whole Kubernetes cluster.

{% hint style="warning" %}
`targetPath` and `selector` are what limit a configuration to a workload. A resource that sets neither applies to every mirrord target in its namespace (or, for `MirrordClusterTlsStealConfig`, in the cluster) that listens on one of its ports, with that configuration's certificates and verification rules, whether or not they fit that workload.
{% endhint %}

An example `MirrordTlsStealConfig` resource that configures HTTPS stealing from an `example-deploy` deployment living in namespace `example-deploy-namespace`:

```yaml
apiVersion: mirrord.metalbear.co/v1alpha
kind: MirrordTlsStealConfig
metadata:
  # The name indicates that this configuration is for the `example-deploy` deployment,
  # but it does not really matter. The mirrord Operator does not inspect config resources' names. 
  name: tls-steal-config-for-example-deploy
  # This is the namespace-scoped variant of the configuration resource,
  # so it must live in the same namespace as the `example-deploy` deployment.
  namespace: example-deploy-namespace
spec:
  # A wildcard pattern that will be matched against session target's path.
  #
  # This pattern can contain `*` and `?` characters, where:
  # 1. `*` will match any amount of any characters;
  # 2. `?` will any character once.
  #
  # E.g `deploy/*/container/container-?` will match both `deploy/name/container/container-1` and `deploy/another-name/container/container-2`.
  #
  # mirrord session target path is produced from:
  # 1. Target resource type (e.g deploy, pod, rollout, statefulset, etc.);
  # 2. Target resource name;
  # 3. `container` literal (if the user selected an exact container as the target);
  # 4. Target container name (if the user selected an exact container as the target).
  #
  # Note that the user can target pods of the `example-deploy` deployment either indirectly, by targeting the deployment, or directly.
  # They can also specify an exact target container or not.
  #
  # Optional. Defaults to a pattern that matches everything.
  targetPath: "*/example-deploy*"
  # A label selector that will be matched against session target's labels.
  #
  # Optional. Defaults to a selector that matches everything.
  selector:
    matchLabels:
      app: example-deploy
  # Each port on the target can be configured separately.
  ports:
  # This entry configures HTTPS stealing from port 443.
  - port: 443
    # Configures how the mirrord-agent authenticates itself and verifies the clients (original request senders) when acting as a TLS server.
    agentAsServer:
      # Configures how the server authenticates itself.
      authentication:
        # Path to a PEM file containing a certificate chain to use.
        #
        # This file must contain at least one certificate.
        # It can contain entries of other types, e.g private keys, which are ignored.
        # Certificates are expected to be listed from the end-entity to the root.
        certPem: /path/to/server/cert.pem
        # Path to a PEM file containing a private key matching the certificate chain from `certPem`.
        #
        # This file must contain exactly one private key.
        # It can contain entries of other types, e.g certificates, which are ignored.
        keyPem: /path/to/server/key.pem
      # ALPN protocols supported by the server, in order of preference.
      #
      # If empty, ALPN is disabled.
      #
      # Optional. Defaults to an empty list.
      alpnProtocols:
      - h2
      - http/1.1
      - http/1.0
      # Configures how the server verifies the clients.
      #
      # Optional. If not present, the server will not offer TLS client authentication at all.
      verification:
        # Whether anonymous clients should be accepted.
        #
        # Optional. Defaults to false.
        allowAnonymous: false
        # Whether the server should accept any certificate, regardless of its validity and who signed it.
        #
        # Note that this setting does not affect whether anonymous clients are accepted or not.
        # If `allowAnonymous` is not set, some certificate will still be required.
        #
        # Optional. Defaults to false.
        acceptAnyCert: false
        # Paths to PEM files and directories PEM files containing allowed root certificates.
        #
        # Directories are not traversed recursively.
        #
        # Each certificate found in the files is treated as an allowed root.
        # The files can contain entries of other types, e.g private keys, which are ignored.
        #
        # Optional. Defaults to an empty list.
        trustRoots:
        - /path/to/trusted/client/root/cert.pem
    # Configures how the mirrord-agent authenticates itself and verifies the server (original request destination) when acting as a TLS client.
    agentAsClient:
      # Configures how the client authenticates itself.
      #
      # Optional. If not present, the client will make the connections anonymously.
      authentication:
        # Path to a PEM file containing a certificate chain to use.
        #
        # This file must contain at least one certificate.
        # It can contain entries of other types, e.g private keys, which are ignored.
        # Certificates are expected to be listed from the end-entity to the root.
        certPem: /path/to/client/cert.pem
        # Path to a PEM file containing a private key matching the certificate chain from `certPem`.
        #
        # This file must contain exactly one private key.
        # It can contain entries of other types, e.g certificates, which are ignored.
        keyPem: /path/to/client/key.pem
      # Configures how the client verifies the server.
      verification:
        # Whether to accept any certificate, regardless of its validity and who signed it.
        #
        # Optional. Defaults to false.
        acceptAnyCert: false
        # Name the server's certificate is verified against, and sent in the SNI extension, when
        # the original client sent no SNI.
        #
        # Set it when the certificate is issued for a DNS name and the original clients do not
        # send SNI (for example an nginx upstream): without it, mirrord-agent verifies the
        # certificate against the pod's IP address, which such a certificate does not cover.
        #
        # Optional. mirrord-agent uses the SNI from the original client, then this name, then
        # the request's host, then the pod's IP address.
        serverName: example-deploy
        # Paths to PEM files and directories PEM files containing allowed root certificates.
        #
        # Directories are not traversed recursively.
        #
        # Each certificate found in the files is treated as an allowed root.
        # The files can contain entries of other types, e.g private keys, which are ignored.
        #
        # Optional. Defaults to an empty list.
        trustRoots:
        - /path/to/trusted/server/root/cert.pem
```

Each `MirrordTlsStealConfig`/`MirrordClusterTlsStealConfig` resource configures HTTPS stealing for some set of available mirrord targets. With the use of `spec.targetPath` and `spec.selector`, you can link one configuration resource to multiple pods, deployments, rollouts, etc.

When the mirrord Operator finds multiple configuration resources matching the session target path and labels, it merges their `ports` lists. The same port cannot be configured multiple times (extra entries are discarded).

**Important:** mirrord-agent will search for all files and directories referenced by the config resources in the target container filesystem.

### Which certificate the application sees

Requests that are not stolen are not forwarded byte for byte. mirrord-agent terminates the caller's TLS connection to read the request, then opens its own TLS connection to the application using `agentAsClient`. When `agentAsClient.authentication` is configured, the application sees that certificate; otherwise, the agent connects without a client certificate. The application never sees the original caller's certificate: the agent does not hold the caller's private key, so it cannot present that certificate. If the application authorizes callers by certificate name, give `agentAsClient` a certificate the application accepts. Pointing it at the application's own certificate makes the application see itself as the caller, which such an allowlist typically rejects with a 403.

mirrord-agent reads the configuration when it starts, and the Operator reuses a running agent for new sessions on the same target. After changing `agentAsClient`, stop the sessions on that target and wait for its `mirrord-agent` pod to exit before starting new ones, otherwise they keep the old certificate.

## Configuring delivery of stolen HTTPS to your local application

By default, when delivering stolen HTTPS requests to your local application, mirrord uses the original protocol - TLS. The connection is made from your local machine by an anonymous TLS client that **does not** verify the server certificate.

This behavior can be configured in your mirrord config with [`feature.network.incoming.tls_delivery`](https://metalbear.com/mirrord/docs/config#feature.network.incoming.tls_delivery). The older `feature.network.incoming.https_delivery` option is deprecated, use `tls_delivery` instead.

If your local application requires a client certificate (mutual TLS), set `tls_delivery.client_cert` and `tls_delivery.client_key` to a certificate it accepts. Without them the handshake fails with a `BadCertificate` alert and every stolen request is answered with a 502.

```json
{
  "feature": {
    "network": {
      "incoming": {
        "tls_delivery": {
          "protocol": "tls",
          "client_cert": "/path/to/client/cert.pem",
          "client_key": "/path/to/client/key.pem"
        }
      }
    }
  }
}
```

The same two settings apply to [preview environments](../../use-cases/preview-environments.md). There the mirrord Operator makes the TLS connection to the preview pod, so `mirrord preview start` reads both files and stores them in the session's Secret, next to `secret_mounts`, and the Operator presents the certificate when it delivers stolen requests. `server_name` is honored too. The other `tls_delivery` settings do not apply to previews: delivery is always TLS, and the preview pod's certificate is not verified.
