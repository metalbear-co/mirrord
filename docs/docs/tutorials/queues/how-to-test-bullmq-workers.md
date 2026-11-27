---
title: How to Test BullMQ Workers in Kubernetes with Queue Splitting
description: Test a changed BullMQ worker in Kubernetes against a shared Redis queue with mirrord queue splitting, with no changes to the worker code.
tags:
  - team
  - enterprise
  - alpha
---

# How to Test BullMQ Workers in Kubernetes with Queue Splitting

This tutorial shows how to run a BullMQ worker on your machine against the real queue in a shared staging cluster, with `mirrord up`. Your local worker gets only the jobs that carry your session key. The deployed worker continues to process all other jobs.

You deploy a small sample app: one Redis instance, one Node.js worker, and one producer script. Then you start a mirrord session with a session key, and you see which jobs go to your machine and which jobs go to the cluster.

mirrord queue splitting needs no changes to the worker code. You do not add an SDK, a wrapper, or a routing check to the worker. The mirrord operator moves the jobs between queues in Redis, and it removes its changes when the session ends.

**Estimated time:** 20 minutes.

## How BullMQ changes the isolation problem

BullMQ keeps each queue in Redis, under keys with the prefix `bull:<queue>:`. A worker takes the next job from the wait list of the queue. When two workers read the same queue, they compete for the jobs. If you start a changed worker on your machine against the staging queue, your worker takes jobs that belong to your colleagues, and the deployed worker loses them.

A private queue does not solve the problem. The producers in staging write to the original queue name, so a private queue gets no jobs unless you also change and redeploy each producer.

mirrord queue splitting keeps the producers and the queue name as they are. The operator takes the jobs from the original queue and puts each job into a different queue, based on the filters of the active sessions. The filter reads the `data` of the job, which is the object that the producer gives to `queue.add()`.

## What you will deploy

All resources go into the namespace `bullmq-demo`:

| Resource | Purpose |
|---|---|
| `redis` Deployment and Service | The Redis backend for BullMQ. One replica, no persistence. |
| `bullmq-app` ConfigMap | The worker script `worker.js` and the producer script `producer.js`. |
| `bullmq-worker` Deployment | The deployed worker. It reads the queue name from the environment variable `BULLMQ_QUEUE`. |
| `bullmq-config` MirrordPropertyList | The Redis connection that the operator uses. |
| `worker-split` MirrordSplitConfig | Tells the operator which workload to split and where its queue name is. |

The pods use the public `node:20-alpine` and `redis:7-alpine` images. You do not build a container image. The worker pod installs the `bullmq` package from npm when it starts, so the cluster needs access to the npm registry.

## Prerequisites

- A Kubernetes cluster for development or staging, version `1.30` or later. Do not use a production cluster.
- `kubectl`, configured for that cluster.
- Node.js and npm on your machine.
- The mirrord CLI, version `3.243.0` or later. From this version, `mirrord up` tells the operator to skip the queue types that it does not split.
- The mirrord operator, version `3.183.0` or later, with a mirrord for Teams license. Queue splitting with `mirrord up` needs this version. Queue splitting is a mirrord for Teams feature.
- BullMQ splitting turned on in the operator Helm chart, with the value `operator.bullmqSplitting`.

To install the operator with BullMQ splitting, run these commands:

```bash
helm repo add metalbear https://metalbear-co.github.io/charts
helm install mirrord-operator metalbear/mirrord-operator \
  --set license.key=<your-license-key> \
  --set operator.bullmqSplitting=true
```

If the operator is already installed, keep its current values and turn on BullMQ splitting:

```bash
helm upgrade mirrord-operator metalbear/mirrord-operator \
  --reuse-values \
  --set operator.bullmqSplitting=true
```

Make sure that the `MirrordSplitConfig` CRD is in the cluster:

```bash
kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co
```

## Deploy the demo

### 1. Create the namespace and Redis

Save this file as `redis.yaml`:

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: bullmq-demo
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: redis
  namespace: bullmq-demo
spec:
  replicas: 1
  selector:
    matchLabels:
      app: redis
  template:
    metadata:
      labels:
        app: redis
    spec:
      containers:
        - name: redis
          image: redis:7-alpine
          args: ["--save", "", "--appendonly", "no"]
          ports:
            - containerPort: 6379
---
apiVersion: v1
kind: Service
metadata:
  name: redis
  namespace: bullmq-demo
spec:
  selector:
    app: redis
  ports:
    - port: 6379
      targetPort: 6379
```

The operator moves jobs with the Redis command `BLMOVE`, which needs Redis 6.2 or later. The `redis:7-alpine` image meets this requirement.

### 2. Create the worker and the producer

The worker prints the queue that it reads when it starts. For each job, it prints the job ID, the job name, and the `tenant` and `orderId` fields of the job data.

Save this file as `worker.js`:

```javascript
const { Worker } = require('bullmq');

const queueName = process.env.BULLMQ_QUEUE;
const connection = {
  host: process.env.REDIS_HOST,
  port: Number(process.env.REDIS_PORT || 6379),
};

console.log(`worker reads queue "${queueName}"`);

const worker = new Worker(
  queueName,
  async (job) => {
    console.log(
      `job ${job.id} name=${job.name} tenant=${job.data.tenant} order=${job.data.orderId}`,
    );
  },
  { connection },
);

worker.on('failed', (job, err) => {
  console.error(`job ${job ? job.id : '?'} failed: ${err.message}`);
});

async function shutdown() {
  await worker.close();
  process.exit(0);
}

process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
```

The producer adds jobs to the queue `orders`. Each job carries a `tenant` field in its data. Jobs for a tenant other than `none` also carry a `baggage` field with `mirrord-session=<tenant>`. The filter of `mirrord up` uses this field. The first argument sets the tenant, and the second argument sets the number of jobs.

Save this file as `producer.js`:

```javascript
const { Queue } = require('bullmq');

const [tenant = 'none', count = '3'] = process.argv.slice(2);
const connection = {
  host: process.env.REDIS_HOST,
  port: Number(process.env.REDIS_PORT || 6379),
};

// The producer writes to the original queue name. It does not read
// BULLMQ_QUEUE, because the operator changes that variable during a split.
const queue = new Queue('orders', { connection });

(async () => {
  for (let i = 1; i <= Number(count); i++) {
    const orderId = `${tenant}-${Date.now()}-${i}`;
    // mirrord up sends a job to the session whose key is in "baggage".
    const data = tenant === 'none'
      ? { tenant, orderId }
      : { tenant, orderId, baggage: `mirrord-session=${tenant}` };
    await queue.add('process-order', data);
    console.log(`sent order=${orderId} tenant=${tenant}`);
  }
  await queue.close();
})();
```

Put both scripts into a ConfigMap:

```bash
kubectl apply -f redis.yaml
kubectl -n bullmq-demo create configmap bullmq-app \
  --from-file=worker.js --from-file=producer.js
```

### 3. Deploy the worker

The worker container copies the scripts to `/app`, installs `bullmq`, and starts the worker. The queue name comes from the environment variable `BULLMQ_QUEUE`. The operator changes this variable during a split, so the variable must be in the pod template.

Save this file as `worker.yaml`:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: bullmq-worker
  namespace: bullmq-demo
spec:
  replicas: 1
  selector:
    matchLabels:
      app: bullmq-worker
  template:
    metadata:
      labels:
        app: bullmq-worker
    spec:
      containers:
        - name: worker
          image: node:20-alpine
          command: ["sh", "-c"]
          args:
            - >
              mkdir -p /app && cp /src/*.js /app/ && cd /app &&
              npm install --no-save --silent bullmq@5 &&
              exec node worker.js
          env:
            - name: REDIS_HOST
              value: redis.bullmq-demo.svc.cluster.local
            - name: REDIS_PORT
              value: "6379"
            - name: BULLMQ_QUEUE
              value: orders
          volumeMounts:
            - name: app
              mountPath: /src
      volumes:
        - name: app
          configMap:
            name: bullmq-app
```

Apply the file and wait for the rollout:

```bash
kubectl apply -f worker.yaml
kubectl -n bullmq-demo rollout status deploy/bullmq-worker
```

### 4. Create the mirrord resources

The operator connects to Redis to move the jobs. The `MirrordPropertyList` gives the connection. The `MirrordSplitConfig` names the target workload, the queue ID that a custom filter uses, and the environment variable that holds the queue name.

Save this file as `split.yaml`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: bullmq-config
  namespace: bullmq-demo
spec:
  properties:
    - name: url
      value: redis://redis.bullmq-demo.svc.cluster.local:6379
---
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: worker-split
  namespace: bullmq-demo
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: bullmq-worker
  clientConfigs:
    bullMq: bullmq-config
  queues:
    - id: orders
      kind: bullMq
      appConfig:
        queue:
          - env: BULLMQ_QUEUE
```

Apply the file:

```bash
kubectl apply -f split.yaml
```

## Confirm the baseline

Before you start a session, make sure that the deployed worker gets all jobs.

Open a terminal and follow the worker logs:

```bash
kubectl -n bullmq-demo logs -f deploy/bullmq-worker
```

The pod installs `bullmq` before it starts the worker. Wait until the worker prints `worker reads queue "orders"`. Then, in a second terminal, send jobs for two tenants. The producer runs in the worker pod:

```bash
kubectl -n bullmq-demo exec deploy/bullmq-worker -c worker -- node /app/producer.js alice 2
kubectl -n bullmq-demo exec deploy/bullmq-worker -c worker -- node /app/producer.js carol 2
```

The deployed worker prints all four jobs. The IDs and order numbers on your cluster are different:

```text
worker reads queue "orders"
job 1 name=process-order tenant=alice order=alice-1795610589033-1
job 2 name=process-order tenant=alice order=alice-1795610589212-2
job 3 name=process-order tenant=carol order=carol-1795610590023-1
job 4 name=process-order tenant=carol order=carol-1795610590056-2
```

The deployed worker processes the jobs in the order that the producer adds them.

## Create sessions and test

### 1. Prepare the local worker

On your machine, create a directory for the local worker. Copy the same `worker.js` into it, and install `bullmq`:

```bash
mkdir bullmq-local && cd bullmq-local
cp ../worker.js .
npm init -y > /dev/null
npm install bullmq@5
```

You can change `worker.js` here. For this tutorial, keep the same code. The local worker does not need a Redis address or a queue name. mirrord gives it the environment of the deployed pod, and the operator sets `BULLMQ_QUEUE` to the queue of your session.

### 2. Write the mirrord configuration

Save this file as `mirrord-up.yaml` in `bullmq-local`:

```yaml
common:
  operator: true
services:
  bullmq-worker:
    target:
      path: deployment/bullmq-worker/container/worker
      namespace: bullmq-demo
    run:
      command: ["node", "worker.js"]
```

The file has one service, `bullmq-worker`, that runs `worker.js` against container `worker` of the `bullmq-worker` Deployment. The file has no queue configuration. `mirrord up` splits the queues of the target automatically. Your local worker gets only the jobs with a string value in the job data that contains `mirrord-session=<key>`, where `<key>` is your session key.

### 3. Start the session

Start the local worker with `mirrord up` and the session key `alice`:

```bash
mirrord up --key alice
```

When the first session starts, the operator restarts the `bullmq-worker` pod with a different value of `BULLMQ_QUEUE`. The operator does not change the Deployment spec, so `kubectl rollout status` does not wait for this restart. `mirrord up` starts the local worker after the operator restarts the pod. This can take one minute or more.

Wait until the local worker prints its queue name. `mirrord up` puts the service name in front of each line of its output and of the worker output. The end of the output is:

```text
bullmq-worker: Session key: alice
bullmq-worker: If enabled, a `mirrord-key` header with this value will be injected into redirected HTTP requests and their responses, and into the queue-split messages routed to this session.
bullmq-worker: config summary
bullmq-worker: running process
bullmq-worker: Ready!
bullmq-worker: worker reads queue "mirrord-tmp-<random>-orders"
```

The old pod is gone, so stop the `kubectl logs -f` command and start it again:

```bash
kubectl -n bullmq-demo logs -f deploy/bullmq-worker
```

The new pod installs `bullmq` again before it starts the worker. Then it prints its queue name:

```text
worker reads queue "mirrord-tmp-<random>-main-orders"
```

The two workers now read different temporary queues. The two random parts are different.

### 4. Send jobs with and without a match

Send two jobs for `alice` and two jobs for `carol`:

```bash
kubectl -n bullmq-demo exec deploy/bullmq-worker -c worker -- node /app/producer.js alice 2
kubectl -n bullmq-demo exec deploy/bullmq-worker -c worker -- node /app/producer.js carol 2
```

The local worker prints only the jobs for `alice`:

```text
bullmq-worker: worker reads queue "mirrord-tmp-<random>-orders"
bullmq-worker: job 2 name=process-order tenant=alice order=alice-1795610935899-2
bullmq-worker: job 1 name=process-order tenant=alice order=alice-1795610935843-1
```

The deployed worker prints only the jobs for `carol`:

```text
worker reads queue "mirrord-tmp-<random>-main-orders"
job 2 name=process-order tenant=carol order=carol-1795610936551-2
job 1 name=process-order tenant=carol order=carol-1795610936474-1
```

The job IDs start again at 1, because each temporary queue has its own ID counter. The operator copies the job name and the job data. It also adds a `mirrord-key` field to the data of each job that goes to your session. Your worker sees this field in `job.data`.

A job can take up to 5 seconds to arrive, because the operator does not wake the worker. A BullMQ v5 worker with no jobs reads the queue again after its `drainDelay` (default 5 seconds). This applies to the local worker and to the deployed worker.

When more than one job waits in a temporary queue, the worker takes the newest job first. In the output above, job 2 starts before job 1. The order of the job IDs is the order of the original queue.

### 5. Add a second developer

Many sessions can split the same queue at the same time. Each session gets its own temporary queue.

Open a new terminal, and go to the `bullmq-local` directory. Start the second session with the same `mirrord-up.yaml` and the key `bob`:

```bash
mirrord up --key bob
```

Send one job for each tenant:

```bash
for t in alice bob carol; do
  kubectl -n bullmq-demo exec deploy/bullmq-worker -c worker -- node /app/producer.js "$t" 1
done
```

The result is:

- The first local worker prints the job for `alice`.
- The second local worker prints the job for `bob`.
- The deployed worker prints the job for `carol`.

The second session does not restart the deployed worker. The operator adds the second filter to the routing that runs already. The second local worker starts after a few seconds and prints its own queue name, such as `mirrord-tmp-<random>-orders`.

{% hint style="warning" %}
Make sure that the filters of different developers do not match the same jobs. Use a different session key for each developer, and do not use a key that is the start of a different key, for example `ali` and `alice`. The filter of the key `ali` also matches the jobs for `alice`. When two filters match a job, the operator sends the job to only one of the two sessions, and you cannot control which session gets it.
{% endhint %}

### 6. Stop the sessions

{% hint style="warning" %}
When a session ends, the operator deletes the Redis keys of the session queue. Let your local worker process its jobs before you stop it. Jobs that stay in your session queue are deleted. Do not send jobs with your session key for 30 seconds after you stop `mirrord up`. During this time, the operator continues to send these jobs to your session queue, and then it deletes them.
{% endhint %}

Press `Ctrl+C` in each terminal that runs `mirrord up`.

The operator ends a session about 30 seconds after you stop its `mirrord up`. After the last session ends, the operator does these steps:

1. It waits for the deployed worker to finish the jobs in the main temporary queue. This includes jobs that wait, jobs that are delayed or prioritized, and jobs that a live worker holds.
2. It restarts the `bullmq-worker` pod with the original queue `orders`.
3. It deletes the Redis keys of the main temporary queue.

To limit the wait in step 1, set `spec.drainTimeout` in seconds on the `MirrordSplitConfig`. When the wait ends with unfinished jobs in the main queue, the operator does not delete that queue. It deletes the queue later, when no job in it is unfinished. The `Drained` condition in the status of the `MirrordSplitConfig` names each queue that the operator keeps:

```bash
kubectl -n bullmq-demo get mirrordsplitconfig worker-split -o yaml
```

The `phase` field in the status is `active` while a session runs, `draining` during the steps above, and `inactive` at the end. Wait for the `inactive` phase:

```bash
kubectl -n bullmq-demo wait mirrordsplitconfig/worker-split \
  --for=jsonpath='{.status.phase}'=inactive --timeout=600s
```

The restored pod installs `bullmq` again before it starts the worker. The producer runs in this pod, so wait until the worker prints its queue name:

```bash
until kubectl -n bullmq-demo logs deploy/bullmq-worker 2>/dev/null | grep -q 'queue "orders"'; do sleep 2; done
```

Send one more job. The restored deployed worker gets it from `orders`:

```bash
kubectl -n bullmq-demo exec deploy/bullmq-worker -c worker -- node /app/producer.js alice 1
kubectl -n bullmq-demo logs deploy/bullmq-worker
```

## How it works

When the first session starts, the operator creates a temporary main queue for the deployed worker. It restarts the pods of the workload with `BULLMQ_QUEUE` set to this queue. It also creates one temporary queue for each session and gives that name to the local worker. A forwarder in the operator takes each job ID from the wait list of the original queue (`bull:orders:wait`). It reads the `data` field of the job hash as JSON, applies the filters of the sessions, and adds the job to the queue of the matching session. A job that no filter matches goes to the main queue. Producers do not change, and they continue to write to `orders`.

```mermaid
flowchart LR
    P[Producers in staging] -->|queue.add| O[(bull:orders:wait)]
    O -->|take job ID| F[mirrord operator forwarder]
    F -->|data has mirrord-session=alice| A[(mirrord-tmp-...-orders<br/>session of alice)]
    F -->|data has mirrord-session=bob| B[(mirrord-tmp-...-orders<br/>session of bob)]
    F -->|no match| M[(mirrord-tmp-...-main-orders)]
    A --> LA[Local worker: alice]
    B --> LB[Local worker: bob]
    M --> D[Deployed bullmq-worker]
```

## Behavior and tradeoffs

- **The filter reads only the job data.** The filter of `mirrord up` is a `jq_filter` that matches a job when a string value anywhere in the parsed `data` JSON contains `mirrord-session=<key>`. The field name does not matter. The filter does not read the job name, the job ID, or the job options. A key that is the start of a different key, for example `ali` and `alice`, matches the jobs of both keys.
- **Custom filters.** In a `config_patch`, `message_filter` matches regular expressions against the top-level string, number, and boolean fields of the job data. Use `jq_filter` for nested values, for example `".tenant == \"alice\" and .order.priority == \"high\""`. When you set both filters, both must match.
- **Jobs with data that is not JSON go to the deployed worker.** They never match a filter.
- **Split is the default.** In the `split` mode of `mirrord up`, a matched job goes only to your local worker. With `mirrord up --mode mirror` (CLI `3.258.0` or later), a matched job goes to your local worker, and a copy goes to the deployed worker.
- **The workload restarts twice.** The operator restarts the pods when the first session starts, and again after the last session ends. The Deployment spec does not change, so `kubectl rollout status` does not show these restarts.
- **Jobs can arrive up to 5 seconds late, and a batch runs newest-first.** See step 4 of the session.
- **The queue name must come from the pod spec.** The operator reads `BULLMQ_QUEUE` only when the value is in the pod template (`value`, or `valueFrom` with a ConfigMap reference) or comes from a ConfigMap with `envFrom`. A mounted config file is also supported. See the [queue splitting overview](../../sharing-the-cluster/queue-splitting.md#queue-names-in-mounted-config-files).
- **The forwarder has tuning values.** With a `queueConfig` property list on the queue entry, you can set `dequeue_timeout` (seconds that the forwarder waits for a new job before it tries again, default `5`) and `jq_time_limit` (milliseconds for each jq evaluation, default `200`). A jq filter that goes over its time limit stops the session.
- **Delayed and prioritized jobs do not split.** The forwarder reads only the wait list of `orders`. A job that a producer adds with a `delay` or `priority` option during a split stays in `orders`, and no worker processes it. After the last session ends, the restored deployed worker processes it, also when its data matches your filter.

## Adapt this to your stack

To use queue splitting with a real BullMQ service, change these items:

1. **The target.** In `targetRef`, set the kind (`Deployment`, `StatefulSet`, or `Rollout`) and the name of your worker workload. In `mirrord-up.yaml`, set `target.path` and `target.namespace` to the same workload and container.
2. **The queue name source.** In `appConfig.queue`, name the environment variable that holds the queue name. If the name has a pattern, use `envLike`. If the workload has more than one container, use `containerNames`. Add one entry to `spec.queues` for each queue that the workload reads.
3. **The Redis connection.** In the `MirrordPropertyList`, use `url`, or `host` with `port`. Add `password`, `tls: "true"`, and `db` as necessary. To share one property list with many namespaces, put it in the namespace of the operator.
4. **The session key in the job data.** Make your producers put `mirrord-session=<key>` in a string field of the job data, for example `baggage`.
5. **The temporary queue names.** If your Redis has naming rules, set `spec.tmpNameTemplate`. The template must contain `{{RANDOM}}`.

The worker code does not change. The worker reads its queue name from the environment, as it does in production.

To use your own filter instead of the session key, set `feature.split_queues` in the `config_patch` of the service. A `config_patch` that sets `split_queues` replaces all the queue splits that `mirrord up` makes for that service. This `mirrord-up.yaml` filters on the `tenant` field. `config_patch` needs mirrord CLI `3.251.0` or later:

```yaml
common:
  operator: true
services:
  bullmq-worker:
    target:
      path: deployment/bullmq-worker/container/worker
      namespace: bullmq-demo
    run:
      command: ["node", "worker.js"]
    config_patch:
      feature:
        split_queues:
          orders:
            queue_type: BullMQ
            message_filter:
              tenant: "^{{ key }}$"
```

The key `orders` is the queue `id` from the `MirrordSplitConfig`. With mirrord `3.264.0` and operator `3.212.0` or later, you can also use the `filter` field with `any_of` and `all_of` lists.

## Cleanup

{% hint style="warning" %}
Stop all mirrord sessions before you delete the resources. If the `MirrordPropertyList` is deleted before the operator finishes the drain, the operator cannot connect to Redis, and you must delete the temporary keys yourself.
{% endhint %}

Delete the demo namespace. This deletes Redis, the worker, and the mirrord resources:

```bash
kubectl delete namespace bullmq-demo
```

`mirrord up` starts a local mirrord daemon, and the daemon continues to run after the sessions end. Stop it:

```bash
mirrord ui stop
```

Go back to the parent directory of `bullmq-local`, and delete it:

```bash
cd ..
rm -rf bullmq-local
```

If you used queue splitting on a Redis instance that you keep, look for temporary keys that remain:

```bash
redis-cli --scan --pattern 'bull:mirrord-tmp-*'
```

## Frequently asked questions

### Can I test a BullMQ worker locally against a staging Redis without stealing jobs from other developers?

Yes. Start the local worker with `mirrord up` and a session key. The operator sends only the jobs with your session key in the job data to your worker. All other jobs go to the deployed worker or to the sessions of other developers.

### Can mirrord filter BullMQ jobs by job name?

No. The filter reads only the `data` field of the job. The job name, the job ID, and the job options are not available to the filter. Add the value to the job data, and filter on that field in a `config_patch`.

### Do I need to change my BullMQ worker or producer code?

No. The operator changes the `BULLMQ_QUEUE` environment variable of the deployed worker and of your local worker. The producers continue to write to the original queue name. The worker must read its queue name from an environment variable or from a mounted config file.

### What happens to my jobs when I stop mirrord?

The operator deletes your session queue when the session ends. Jobs that stay in it are deleted, so let your worker process them first. Jobs in the main queue of the deployed worker stay until the deployed worker finishes them.

### Can my teammate and I test the same BullMQ queue at the same time?

Yes. Each session gets its own temporary queue, and the operator applies all filters. Use a different session key for each developer. When two filters match a job, the operator sends it to only one of the two sessions, and you cannot control which session gets it.

### How does my local worker know which session a job belongs to?

When the operator setting `operator.injectSessionKeyHeader` is on (the default), the operator adds a `mirrord-key` field to the `data` object of each job that it sends to your session. The job that the deployed worker gets does not change. The operator adds the field only when the data is a JSON object. A filter does not match data that is not JSON. Such a job goes to the deployed worker.

## See also

- [BullMQ queue splitting reference](../../sharing-the-cluster/queue-splitting/bullmq.md)
- [Queue splitting overview](../../sharing-the-cluster/queue-splitting.md)
- [Setting a filter for a mirrord run](../../sharing-the-cluster/queue-splitting.md#setting-a-filter-for-a-mirrord-run)
- [`feature.split_queues` configuration reference](https://metalbear.com/mirrord/docs/config/options#feature-split_queues)
- [Queue splitting status](../../sharing-the-cluster/queue-splitting/status.md)
