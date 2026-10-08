# Common Issues

> Sources:
> - https://metalbear.com/mirrord/docs/getting-started/installing-mirrord/operator
> - https://metalbear.com/mirrord/docs/troubleshooting

## "Operator not found" when running mirrord

The mirrord client cannot find or connect to the operator in the cluster.

**Solution:** Confirm the operator is up and reachable:

```bash
mirrord operator status                        # preferred — checks the client can reach the operator
kubectl get deployment -n mirrord mirrord-operator
kubectl get pods -n mirrord
```

If the pod isn't running, inspect it:

```bash
kubectl describe deployment -n mirrord mirrord-operator
kubectl logs -n mirrord deployment/mirrord-operator
```

## Licensing / authentication errors

The operator can't obtain or validate a license.

**How the operator authenticates (know which path you're on):**
- **Cloud API key (default):** the operator exchanges a cloud API key for a license over the API. Provide it via `cloud.apiKey.keyRef` (Secret), `cloud.apiKey.gsmRef` (Google Secret Manager), or `cloud.apiKey.key` (inline dev/test). A revoked/rotated key or no cloud connectivity causes failures — regenerate in the dashboard (Settings, app.metalbear.com).
- **License key (deprecated for cloud):** `license.key` / `license.keyRef` (Secret data key `OPERATOR_LICENSE_KEY`). Still required as the shared secret when using a self-hosted license server.
- **Air-gapped:** offline PEM (`license.file.secret.data.license.pem` or `license.pemRef`) or a `license.licenseServer` URL.

**Checks:**

```bash
# If using a Secret ref, confirm it exists and is populated
kubectl get secret -n mirrord <your-secret-name>

# Look for auth/license errors in the logs
kubectl logs -n mirrord deployment/mirrord-operator | grep -iE 'licen|api key|cloud|auth'
```

To rotate a **cloud API key**, update it in the dashboard, recreate the Secret (have the user run it — never paste the key into the agent), and `helm upgrade`. Never pass key material via `--set`.

## "Permission denied" / users can't create sessions (RBAC)

Users lack permission on mirrord's CRDs.

**Solution:** Use the **roles the chart creates** — don't hand-write a role for mirrord's CRDs (they span several API groups: `operator.metalbear.co`, `mirrord.metalbear.co`, `queues.mirrord.metalbear.co`, `preview.mirrord.metalbear.co`, and change over time).

1. For each namespace where developers run mirrord, add it to `roleNamespaces` so the chart creates a namespaced role there:
   ```yaml
   roleNamespaces:
     - staging
     - development
   ```
2. `helm upgrade`, then bind users/groups/service accounts to the chart's roles with your own RoleBinding/ClusterRoleBinding. Available cluster roles: `mirrord-operator-user-basic`, `mirrord-operator-user`, and `mirrord-operator-ci` (scoped for CI / preview environments). Example:
   ```yaml
   apiVersion: rbac.authorization.k8s.io/v1
   kind: ClusterRoleBinding
   metadata:
     name: alice-mirrord
   subjects:
     - kind: User
       name: alice
       apiGroup: rbac.authorization.k8s.io
   roleRef:
     kind: ClusterRole
     name: mirrord-operator-user
     apiGroup: rbac.authorization.k8s.io
   ```

`roleNamespaces: []` (empty) creates no namespaced roles.

## Operator pod keeps crashing or restarting

```bash
kubectl logs -n mirrord deployment/mirrord-operator --previous
kubectl get events -n mirrord --sort-by='.lastTimestamp'
```

Raise resources if the cluster is large (defaults suit ~200 concurrent sessions):

```yaml
operator:
  requests: { cpu: 100m, memory: 100Mi }
  limits:   { cpu: 500m, memory: 200Mi }   # increase for more sessions
```

## Port 443 conflict or unavailable

The operator can't bind port 443.

```yaml
operator:
  port: 8443   # or 3000
```

Ensure nodes can reach the chosen port, then `helm upgrade`.

## `mirrord operator status` fails with `503 Service Unavailable` on GKE

With private networking, firewall rules may block the operator's API service.

**Solution:** Add a firewall rule allowing your cluster's master nodes to reach the operator port (default 443) on the cluster's pods.

## TLS / API-service certificate errors

The API server can't validate the operator's aggregated API service.

**Solution:**
- If you use a verified certificate, set `tls.apiService.insecureSkipTLSVerify: false`.
- To provision the cert out-of-band (ExternalSecret, cert-manager), set `tls.useExistingSecret: true` (then `tls.data`/`tls.certManager` are ignored), or enable `tls.certManager.enabled: true`.
- Restart to regenerate a self-signed cert if neither cert-manager nor `tls.data` is set:
  ```bash
  kubectl rollout restart deployment -n mirrord mirrord-operator
  ```

## Sessions fail silently with a service mesh

Meshes (Istio, Linkerd) can interfere with operator↔agent traffic.

**Solution:** Exclude the mirrord namespace from the mesh, or pin a static agent port and exclude it:

```yaml
agent:
  port: 50000
```

Istio target-pod annotation:

```
traffic.sidecar.istio.io/excludeInboundPorts: '50000'
```

## Feature enabled in config but rejected by the operator

A feature (queue splitting, DB branching, preview environments) is used client-side but its Helm flag isn't enabled, or the operator/CLI version is too old.

**Solution:** Enable the matching `operator.*` flag (e.g. `operator.kafkaSplitting`, `operator.pgBranching`, `operator.previewEnv`) and `helm upgrade`. Check each feature's minimum operator/CLI/chart versions in the corresponding feature skill (`mirrord-kafka`, `mirrord-db-branching`, `mirrord-prev-env`). Note **generic DB branching** (`operator.genericBranching`) lets branch creators run arbitrary images — gate it and restrict `genericBranchConfig.dbPod.allowedImages`.

## Upgrading the operator breaks in-flight sessions

Running sessions can fail during an upgrade.

**Solution:** Upgrade when the cluster is quiet. Check activity with `mirrord operator status`, then:

```bash
helm repo update
helm upgrade --install -f values.yaml mirrord-operator metalbear/mirrord-operator
```
