# mirrord-agent
[![CI](https://github.com/metalbear-co/mirrord/actions/workflows/ci.yaml/badge.svg)](https://github.com/metalbear-co/mirrord/actions/workflows/ci.yaml)

Agent part of [mirrord](https://github.com/metalbear-co/mirrord) responsible for running on the same node as the debuggee, entering it's namespace and collecting traffic.

mirrord-agent is written in Rust for safety, low memory consumption and performance.

mirrord-agent is distributed as a container image (currently only x86) that is published on [GitHub Packages publicly](https://github.com/metalbear-co/mirrord-agent/pkgs/container/mirrord-agent). 

## Enabling prometheus metrics

To start the metrics server, you'll need to add this config to your `mirrord.json`:

```json
{
  "agent": {
    "metrics": "0.0.0.0:9000",
    "annotations": {
      "prometheus.io/scrape": "true",
      "prometheus.io/port": "9000"
    }
}
```

Remember to change the `port` in both `metrics` and `annotations`, they have to match,
otherwise prometheus will try to scrape on `port: 80` or other commonly used ports.

### Installing prometheus

Run `kubectl apply -f {file-name}.yaml` on these sequences of `yaml` files and you should
get prometheus running in your cluster. You can access the dashboard from your browser at
`http://{cluster-ip}:30909`, if you're using minikube it might be
`http://192.168.49.2:30909`.

You'll get prometheus running under the `monitoring` namespace, but it'll be able to look
into resources from all namespaces. The config in `configmap.yaml` sets prometheus to look
at pods only, if you want to use it to scrape other stuff, check
[this example](https://github.com/prometheus/prometheus/blob/main/documentation/examples/prometheus-kubernetes.yml).

1. `create-namespace.yaml`

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: monitoring
```

2. `cluster-role.yaml`

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: prometheus
rules:
- apiGroups: [""]
  resources:
  - nodes
  - services
  - endpoints
  - pods
  verbs: ["get", "list", "watch"]
- apiGroups:
  - extensions
  resources:
  - ingresses
  verbs: ["get", "list", "watch"]
```

3. `service-account.yaml`

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: prometheus
  namespace: monitoring
```

4. `cluster-role-binding.yaml`

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: prometheus
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: prometheus
subjects:
- kind: ServiceAccount
  name: prometheus
  namespace: monitoring
```

5. `configmap.yaml`

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: prometheus-config
  namespace: monitoring
data:
  prometheus.yml: |
    global:
      keep_dropped_targets: 100

    scrape_configs:
      - job_name: "kubernetes-pods"

        kubernetes_sd_configs:
          - role: pod

        relabel_configs:
          - source_labels: [__address__, __meta_kubernetes_pod_annotation_prometheus_io_port]
            action: replace
            regex: ([^:]+)(?::\d+)?;(\d+)
            replacement: $1:$2
            target_label: __address__
          - action: labelmap
            regex: __meta_kubernetes_pod_label_(.+)
          - source_labels: [__meta_kubernetes_namespace]
            action: replace
            target_label: namespace
          - source_labels: [__meta_kubernetes_pod_name]
            action: replace
            target_label: pod
```

- If you make any changes to the 5-configmap.yaml file, remember to `kubectl apply` it
 **before** restarting the `prometheus` deployment.

6. `deployment.yaml`

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: prometheus
  namespace: monitoring
  labels:
    app: prometheus
spec:
  replicas: 1
  strategy:
    rollingUpdate:
      maxSurge: 1
      maxUnavailable: 1
    type: RollingUpdate
  selector:
    matchLabels:
      app: prometheus
  template:
    metadata:
      labels:
        app: prometheus
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/port: "9090"
    spec:
      serviceAccountName: prometheus
      containers:
      - name: prometheus
        image: prom/prometheus
        args:
          - '--config.file=/etc/prometheus/prometheus.yml'
        ports:
        - name: web
          containerPort: 9090
        volumeMounts:
        - name: prometheus-config-volume
          mountPath: /etc/prometheus
      restartPolicy: Always
      volumes:
      - name: prometheus-config-volume
        configMap:
            defaultMode: 420
            name: prometheus-config
```

7. `service.yaml`

```yaml
apiVersion: v1
kind: Service
metadata:
    name: prometheus-service
    namespace: monitoring
    annotations:
        prometheus.io/scrape: 'true'
        prometheus.io/port:   '9090'
spec:
    selector:
        app: prometheus
    type: NodePort
    ports:
    - port: 8080
      targetPort: 9090
      nodePort: 30909
```
