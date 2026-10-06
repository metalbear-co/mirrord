# MirrordPropertyList — Temporal connection reference

The `MirrordPropertyList` tells the mirrord operator how to connect to the Temporal frontend so it can poll the real task queue during a split. It is referenced by name from a `MirrordSplitConfig` (per queue via `clientConfig`, or once via `spec.clientConfigs.temporal`).

- **API version:** `mirrord.metalbear.co/v1`
- **Kind:** `MirrordPropertyList`
- **Namespace:** the target workload's namespace (recommended default), or the operator's namespace to share one config across namespaces (operator **3.191.0+**). A list in the target's namespace wins over a same-named list in the operator's namespace. ConfigMap/Secret references resolve in the namespace the list was found in.

## Spec

`spec.properties` is a list of `{name, value}` or `{name, valueFrom}` entries. `valueFrom` supports `secretKeyRef` and `configMapKeyRef` — always use `secretKeyRef` for credentials.

## Supported properties

| Property        | Description | Required | Default |
| --------------- | ----------- | :------: | :-----: |
| `address`       | Temporal frontend address (`host:port` or a full URL). A bare `host:port` gets its scheme from the `tls` setting. | ✓ | |
| `namespace`     | Temporal namespace the operator polls. | ✓ | |
| `tls`           | Set to `"true"` to connect over TLS. Implied when any `tls*` property below is set. | No | `false` |
| `apiKey`        | Temporal Cloud API key. | No | |
| `tlsCaCert`     | PEM CA bundle used to verify the frontend's certificate when it is not signed by a publicly trusted root. | No | |
| `tlsClientCert` | PEM client certificate presented to a frontend that requires mutual TLS. Requires `tlsClientKey`. | No | |
| `tlsClientKey`  | PEM private key for `tlsClientCert`. Requires `tlsClientCert`. | No | |
| `tlsServerName` | Overrides the domain name the frontend's certificate is verified against, when the dial address does not match it. | No | |

Rules:

- Setting any `tls*` property implies `tls: "true"`.
- `tlsClientCert` and `tlsClientKey` always go together — setting only one fails with an error when the split starts.
- The operator reads the connection settings when a split **starts**. Rotated certificates are picked up by the next split, not by splits already running.
- TLS applies to the **operator → Temporal frontend** connection only. Deployed workers patched into a split connect to the operator's in-cluster Temporal proxy over plaintext gRPC.

## Examples

### Plaintext self-hosted frontend

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: temporal-config
  namespace: workflows
spec:
  properties:
    - name: address
      value: temporal-frontend.temporal.svc.cluster.local:7233
    - name: namespace
      value: default
```

### Temporal Cloud with an API key

Temporal Cloud's certificate is signed by a publicly trusted CA, so only `tls: "true"` and the key are needed:

```yaml
spec:
  properties:
    - name: address
      value: my-namespace.a1b2c.tmprl.cloud:7233
    - name: namespace
      value: my-namespace.a1b2c
    - name: tls
      value: "true"
    - name: apiKey
      valueFrom:
        secretKeyRef:
          name: temporal-cloud-api-key
          key: apiKey
```

### Private CA and mutual TLS

For a frontend whose certificate is signed by a private CA (common self-hosted), set `tlsCaCert`. If the frontend requires **mutual TLS** (Temporal Cloud mTLS auth, or an mTLS-protected self-hosted cluster), also set `tlsClientCert` + `tlsClientKey`. Keep all certificate material in a Kubernetes Secret:

```yaml
spec:
  properties:
    - name: address
      value: temporal-frontend.mycompany.internal:7233
    - name: namespace
      value: production
    - name: tlsCaCert
      valueFrom:
        secretKeyRef:
          name: temporal-client-tls
          key: ca.crt
    - name: tlsClientCert
      valueFrom:
        secretKeyRef:
          name: temporal-client-tls
          key: tls.crt
    - name: tlsClientKey
      valueFrom:
        secretKeyRef:
          name: temporal-client-tls
          key: tls.key
```

Create the Secret from files (never `--from-literal` for key material):

```bash
kubectl create secret generic temporal-client-tls -n workflows \
  --from-file=ca.crt=./ca.crt \
  --from-file=tls.crt=./client.crt \
  --from-file=tls.key=./client.key
```

## Per-queue settings list (`queueConfig`)

A second, separate `MirrordPropertyList` can hold Temporal per-queue options, referenced from a queue entry's `queueConfig` field in the `MirrordSplitConfig`:

| Property | Description | Required | Default |
| --- | --- | :------: | :-----: |
| `max_buffered_tasks` | Maximum number of tasks the operator buffers per virtual task queue. When the limit is reached, a session queue overflows to the main queue. Positive integer to cap; omit or `0` for unlimited. | No | `0` (unlimited) |

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: temporal-queue-settings
  namespace: workflows
spec:
  properties:
    - name: max_buffered_tasks
      value: "1000"
```
