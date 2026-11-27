---
title: How to Test RabbitMQ Consumers in Kubernetes with Queue Splitting
description: Test a changed RabbitMQ consumer against a shared queue in Kubernetes. mirrord queue splitting sends you only the messages that match your filter.
tags:
  - team
  - enterprise
  - alpha
---

# How to Test RabbitMQ Consumers in Kubernetes with Queue Splitting

This tutorial shows how to run a changed RabbitMQ consumer on your machine against a shared staging queue in Kubernetes, with `mirrord up`. With mirrord queue splitting, your local consumer gets only the messages that carry your session key. The deployed consumer keeps all the other messages.

Queue splitting needs no change to the consumer code. You do not add an SDK, a client wrapper, or a routing check to the consumer. The mirrord operator routes the messages, and the consumer reads its queue name from an environment variable, as it does now.

You deploy a small sample app: one RabbitMQ broker, one Python consumer, and a Python script that sends test messages. All the code is in this page. You do not build a container image.

**Estimated time:** 20 minutes.

## How RabbitMQ changes the isolation problem

In RabbitMQ, all the consumers of one queue compete for its messages. The broker gives each message to one consumer only. If you connect a local consumer to the shared `orders` queue, it takes a random part of the messages. The deployed consumer does not get those messages, and other developers lose test messages that they sent.

Queue splitting solves this problem. The operator reads the original queue and sends each message to a temporary queue. Each mirrord session gets its own temporary queue, and the deployed consumer gets one too. Your filter decides which messages go to your temporary queue. Messages that match no filter go to the deployed consumer.

## What you will deploy

All resources go into the namespace `rmq-demo`:

- A `Secret` with the RabbitMQ user name and password.
- A RabbitMQ broker: one replica, no persistent storage, for test use only.
- A `ConfigMap` with two Python scripts: `consumer.py` and `producer.py`.
- The `orders-consumer` Deployment. It runs `consumer.py` from the public `python:3.12-slim` image.
- A `MirrordPropertyList` that tells the operator how to connect to RabbitMQ.
- A `MirrordSplitConfig` that tells the operator which queue the consumer reads, and from which environment variable.

Each test message carries a `baggage` header with a session key, for example `baggage: mirrord-session=alice`. `mirrord up` makes a filter that matches this header. The messages also carry an `x-user` header, so that the consumer can print the user of each message.

## Prerequisites

Before you start, make sure that you have these items:

1. A Kubernetes cluster, version `1.30` or later. Queue splitting needs this version.
2. `kubectl`, with access to the cluster.
3. Python 3 on your machine, with the `pika` package version `1.5.0` or later. Under mirrord, `pika` 1.3.2 does not connect and waits forever.
4. The mirrord CLI, version `3.253.0` or later. From this version, `mirrord up` splits RabbitMQ queues automatically.
5. The mirrord operator, version `3.201.0` or later, on a mirrord Team or Enterprise plan. `mirrord up` needs this version to split RabbitMQ queues. With an older operator, the session runs without RabbitMQ splitting, and mirrord prints a warning.
6. RabbitMQ splitting turned on in the operator Helm chart.

Create a virtual environment in your working directory and install `pika` in it:

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install pika==1.5.0
```

To turn on RabbitMQ splitting, set the `operator.rmqSplitting` Helm value. Run this command in the namespace and kubecontext where you installed the Helm release:

```bash
helm repo update
helm upgrade mirrord-operator metalbear/mirrord-operator \
  --reuse-values \
  --set operator.rmqSplitting=true
```

Make sure that the `MirrordSplitConfig` CRD exists:

```bash
kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co
```

{% hint style="info" %}
The pod of the sample consumer installs `pika` from PyPI when it starts. Your cluster must have outbound internet access. If it does not, build an image with `pika` installed and use that image in the Deployment.
{% endhint %}

## Deploy the demo

Save this file as `rmq-demo.yaml`:

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: rmq-demo
---
apiVersion: v1
kind: Secret
metadata:
  name: rabbitmq-credentials
  namespace: rmq-demo
stringData:
  username: demo
  password: demo-password
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rabbitmq
  namespace: rmq-demo
spec:
  replicas: 1
  selector:
    matchLabels:
      app: rabbitmq
  template:
    metadata:
      labels:
        app: rabbitmq
    spec:
      containers:
        - name: rabbitmq
          image: rabbitmq:3.13-management-alpine
          ports:
            - containerPort: 5672
              name: amqp
          env:
            - name: RABBITMQ_DEFAULT_USER
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: username
            - name: RABBITMQ_DEFAULT_PASS
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: password
---
apiVersion: v1
kind: Service
metadata:
  name: rabbitmq
  namespace: rmq-demo
spec:
  selector:
    app: rabbitmq
  ports:
    - name: amqp
      port: 5672
      targetPort: 5672
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: orders-app
  namespace: rmq-demo
data:
  consumer.py: |
    import json
    import os
    import signal
    import sys
    import time

    import pika

    # Python runs as PID 1 in the pod and ignores SIGTERM without a handler.
    # This handler lets the old pod stop at once when the operator restarts it.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

    LABEL = sys.argv[1] if len(sys.argv) > 1 else "deployed"
    QUEUE = os.environ["QUEUE_NAME"]
    CREDS = pika.PlainCredentials(os.environ["RMQ_USER"], os.environ["RMQ_PASSWORD"])
    PARAMS = pika.ConnectionParameters(host=os.environ["RMQ_HOST"], credentials=CREDS)


    def connect():
        while True:
            try:
                return pika.BlockingConnection(PARAMS)
            except pika.exceptions.AMQPConnectionError:
                print("waiting for RabbitMQ...", flush=True)
                time.sleep(3)


    def handle(channel, method, props, body):
        user = (props.headers or {}).get("x-user")
        order = json.loads(body)
        print(f"[{LABEL}] x-user={user} order={order['id']}", flush=True)
        channel.basic_ack(delivery_tag=method.delivery_tag)


    channel = connect().channel()
    # Default attributes (not durable, not exclusive, no auto-delete) match the
    # temporary queues that the operator declares when no queueConfig is set.
    channel.queue_declare(queue=QUEUE)
    channel.basic_consume(queue=QUEUE, on_message_callback=handle)
    print(f"[{LABEL}] consuming from {QUEUE}", flush=True)
    channel.start_consuming()
  producer.py: |
    import json
    import os
    import sys

    import pika

    # The original queue name. Do not read QUEUE_NAME here: during a split,
    # the operator changes QUEUE_NAME in the consumer pod.
    QUEUE = "orders"

    user = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    creds = pika.PlainCredentials(os.environ["RMQ_USER"], os.environ["RMQ_PASSWORD"])
    params = pika.ConnectionParameters(host=os.environ["RMQ_HOST"], credentials=creds)
    channel = pika.BlockingConnection(params).channel()
    channel.queue_declare(queue=QUEUE)

    for i in range(1, count + 1):
        # mirrord up sends a message to the session whose key is in "baggage".
        headers = {} if user == "none" else {"x-user": user, "baggage": f"mirrord-session={user}"}
        body = json.dumps({"id": f"{user}-{i}", "user": user})
        channel.basic_publish(
            exchange="",
            routing_key=QUEUE,
            body=body,
            properties=pika.BasicProperties(headers=headers),
        )
        print(f"sent {body} with headers {headers}")
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: orders-consumer
  namespace: rmq-demo
spec:
  replicas: 1
  selector:
    matchLabels:
      app: orders-consumer
  template:
    metadata:
      labels:
        app: orders-consumer
    spec:
      containers:
        - name: main
          image: python:3.12-slim
          command: ["sh", "-c", "pip install --quiet --root-user-action=ignore pika==1.5.0 && exec python -u /app/consumer.py"]
          env:
            - name: QUEUE_NAME
              value: orders
            - name: RMQ_HOST
              value: rabbitmq.rmq-demo.svc.cluster.local
            - name: RMQ_USER
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: username
            - name: RMQ_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: password
          volumeMounts:
            - name: app
              mountPath: /app
      volumes:
        - name: app
          configMap:
            name: orders-app
---
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: rmq-demo-cluster
  namespace: rmq-demo
spec:
  properties:
    - name: host
      value: rabbitmq.rmq-demo.svc.cluster.local
    - name: port
      value: '5672'
    - name: username
      valueFrom:
        secretKeyRef:
          name: rabbitmq-credentials
          key: username
    - name: password
      valueFrom:
        secretKeyRef:
          name: rabbitmq-credentials
          key: password
---
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: orders-consumer-split
  namespace: rmq-demo
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: orders-consumer
  queues:
    - id: orders
      kind: rmq
      clientConfig: rmq-demo-cluster
      appConfig:
        queue:
          - env: QUEUE_NAME
            containerNames:
              - main
```

The two mirrord resources at the end do these tasks:

- The `MirrordPropertyList` gives the operator the broker address and credentials. Each `value` must be a string, so the port is `'5672'`, not `5672`.
- The `MirrordSplitConfig` says that container `main` of `orders-consumer` reads one RabbitMQ queue. The queue name is in the `QUEUE_NAME` environment variable. Developers refer to this queue with the ID `orders`.

{% hint style="warning" %}
The operator can read the queue name only from an environment variable with a `value`, or with a `valueFrom` that refers to a ConfigMap, or from `envFrom` with a ConfigMap. Do not put the queue name in a Secret. The credentials in this demo come from a Secret, which is correct, because they are not queue names.
{% endhint %}

Apply the manifests:

```bash
kubectl apply -f rmq-demo.yaml
```

Wait for the two pods to start:

```bash
kubectl -n rmq-demo rollout status deployment/rabbitmq
kubectl -n rmq-demo rollout status deployment/orders-consumer
```

Make sure that the operator accepts the split config. The `Accepted` column must show `True`:

```bash
kubectl -n rmq-demo get mirrordsplitconfigs
```

## Confirm the baseline

Before you start a mirrord session, make sure that the deployed consumer gets all the messages.

Open a terminal and follow the logs of the deployed consumer:

```bash
kubectl -n rmq-demo logs -f deployment/orders-consumer
```

In a second terminal, send two messages for `alice`, two for `bob`, and one with no headers. The producer runs inside the consumer pod, because that pod already has `pika` and the credentials:

```bash
kubectl -n rmq-demo exec deployment/orders-consumer -- python /app/producer.py alice 2
kubectl -n rmq-demo exec deployment/orders-consumer -- python /app/producer.py bob 2
kubectl -n rmq-demo exec deployment/orders-consumer -- python /app/producer.py none 1
```

The deployed consumer prints all five messages:

```
[deployed] consuming from orders
[deployed] x-user=alice order=alice-1
[deployed] x-user=alice order=alice-2
[deployed] x-user=bob order=bob-1
[deployed] x-user=bob order=bob-2
[deployed] x-user=None order=none-1
```

The log also shows `waiting for RabbitMQ...` lines, a `pip` notice, and a `DeprecationWarning` from `pika`. You can ignore them.

## Run your consumer with a session key

Now run the same consumer on your machine. Your local consumer will get only the messages for `alice`.

Copy `consumer.py` from the ConfigMap to your working directory:

```bash
kubectl -n rmq-demo get configmap orders-app -o jsonpath='{.data.consumer\.py}' > consumer.py
```

In a real project, this is your changed consumer code. You can edit `consumer.py` now, for example to print more fields.

Save this file as `mirrord-up.yaml`:

```yaml
common:
  operator: true
services:
  orders-consumer:
    target:
      path: deployment/orders-consumer/container/main
      namespace: rmq-demo
    run:
      command: ["python3", "consumer.py", "{{ key }}"]
```

The file has one service, `orders-consumer`. The service runs `consumer.py` against container `main` of the `orders-consumer` Deployment. `{{ key }}` is the session key. The consumer prints it in front of each line.

The file has no queue configuration. `mirrord up` splits the queues of the target automatically, with a filter on your session key. Your local consumer gets a message only if a header value of the message contains `mirrord-session=<key>`.

Start the local consumer with `mirrord up` and the session key `alice`:

```bash
mirrord up --key alice
```

mirrord gives the local process the environment of the target pod. The operator changes `QUEUE_NAME` to the name of your temporary queue, so the code does not change. Connections to `rabbitmq.rmq-demo.svc.cluster.local` go through the cluster.

When the session starts, the operator patches the `orders-consumer` Deployment, and its pod restarts. The new pod reads its own temporary queue. Your `kubectl logs -f` command stops when the old pod stops. Run it again:

```bash
kubectl -n rmq-demo logs -f deployment/orders-consumer
```

## Send messages and check both sides

Send the same five messages again:

```bash
kubectl -n rmq-demo exec deployment/orders-consumer -- python /app/producer.py alice 2
kubectl -n rmq-demo exec deployment/orders-consumer -- python /app/producer.py bob 2
kubectl -n rmq-demo exec deployment/orders-consumer -- python /app/producer.py none 1
```

Your local consumer prints only the messages for `alice`. `mirrord up` puts the service name in front of each line of the consumer:

```
orders-consumer: [alice] consuming from mirrord-<random>-orders
orders-consumer: [alice] x-user=alice order=alice-1
orders-consumer: [alice] x-user=alice order=alice-2
```

The deployed consumer prints all the other messages:

```
[deployed] consuming from mirrord-main-<random>-orders
[deployed] x-user=bob order=bob-1
[deployed] x-user=bob order=bob-2
[deployed] x-user=None order=none-1
```

Both consumers now print a temporary queue name, not `orders`. The operator reads `orders` and sends each message to one of the temporary queues.

## Add a second developer

More than one developer can split the same queue at the same time. Each session gets its own temporary queue.

Keep the `alice` session open. Open a new terminal in the same directory. You use the same `mirrord-up.yaml` with a different session key.

Start a second local consumer. Activate the virtual environment first, because this is a new terminal:

```bash
source .venv/bin/activate
mirrord up --key bob
```

The split is already active, so the deployed pod does not restart for the second session.

Send the five messages again. Each message now goes to exactly one consumer:

| Message | Consumer |
| ------- | -------- |
| `alice-1`, `alice-2` | Local consumer `[alice]` |
| `bob-1`, `bob-2` | Local consumer `[bob]` |
| `none-1` | Deployed consumer `[deployed]` |

{% hint style="warning" %}
Make sure that each developer uses a different session key. If two filters match a message, only one of the two sessions gets it. You cannot choose which session.
{% endhint %}

The filter looks for `mirrord-session=<key>` anywhere in a header value. Thus, a key that is the start of a different key, for example `ali` and `alice`, also matches the messages of the longer key. Use keys that are not the start of other keys.

You do not need a different directory for each session. Two `mirrord up` runs with different keys can use the same `mirrord-up.yaml` in the same directory at the same time.

## Stop the session

Stop the `bob` consumer with `Ctrl+C`. Then stop the `alice` consumer with `Ctrl+C`.

When you stop the `bob` session, the `alice` session continues. The operator keeps the `bob` session for about 30 seconds after `Ctrl+C`. In that time, new messages for `bob` still go to the temporary queue of `bob`. Then the operator deletes that queue, and new messages for `bob` go to the deployed consumer again, because no filter matches them. Unread messages in the deleted queue are lost. The operator does not send them to the deployed consumer.

When the last session ends, the operator removes the split. In this demo, the operator started to remove the split about 30 seconds after `Ctrl+C`. The deployed pod then restarts. The operator deletes the temporary queues, and the deployed consumer reads `orders` again.

Before the operator removes the split, it drains the temporary queue of the deployed consumer. The `spec.drainTimeout` field of the `MirrordSplitConfig` sets the limit on this drain:

| `drainTimeout` | Behavior |
| -------------- | -------- |
| Not set | The operator waits until the deployed consumer reads all messages in its temporary queue. There is no time limit. |
| `0` | The operator removes the split immediately. Unread messages can be lost. |
| `N` | The operator waits up to `N` seconds for the deployed consumer to read its temporary queue, then removes the split. |

To keep the split for a new session after the last session ends, set `spec.ttl` (in seconds) on the `MirrordSplitConfig`. A new session in that time uses the same temporary resources. The `ttl` period runs before the drain. To keep the split for 60 seconds in the demo, add `ttl: 60` under `spec` in the `MirrordSplitConfig`.

{% hint style="warning" %}
Do not stop a session while your local consumer has unread messages that you need. The operator deletes your temporary queue and the messages in it.
{% endhint %}

## How it works

When the first session starts, the operator creates two temporary queues: one for the deployed consumer and one for your local consumer. It patches the target workload so that `QUEUE_NAME` holds the name of the deployed consumer's temporary queue. Your local consumer gets the name of its own temporary queue. The operator then reads the original queue and sends each message to the temporary queue whose filter matches. Messages that match no filter go to the deployed consumer's temporary queue. Each new session adds one more temporary queue and one more filter.

![Two RabbitMQ splitting sessions on the orders queue](../../.gitbook/assets/rabbitmq-tutorial.svg)

## Behavior and tradeoffs

- **The queue name must come from the environment or a mounted file.** The operator changes the queue name in an environment variable, or in a config file mounted from a ConfigMap or Secret volume. A queue name that is hard-coded in the code cannot be split.
- **The environment variable must be readable by the operator.** It must have a `value`, a `valueFrom` with a ConfigMap reference, or come from `envFrom` with a ConfigMap.
- **The deployed consumer restarts.** When a split starts, the operator patches the workload, so its pods restart. When the split ends, they restart again.
- **Supported workloads.** The `targetRef` can be a `Deployment`, a `StatefulSet`, or an Argo `Rollout`.
- **The filter of `mirrord up`.** `mirrord up` makes a `jq_filter` for each session. The filter matches a message when a string value in its headers contains `mirrord-session=<key>`. The header name does not matter. The filter does not read the body.
- **Overlapping filters.** If two sessions match the same message, only one of them gets it. A key that is the start of a different key, for example `ali` and `alice`, matches the messages of both keys.
- **Queue attributes.** By default, the operator declares temporary queues that are not durable, not exclusive, and not auto-delete. If your consumer declares its queue with other attributes, for example `durable` or `x-queue-type: quorum`, put those attributes in a `MirrordPropertyList` and set `queueConfig` on the queue entry.
- **Unread messages at teardown.** When the last session ends, the operator waits until the deployed consumer reads its temporary queue. Set `drainTimeout` to limit this wait. Unread messages in a session's temporary queue are lost when that session ends.
- **Message order.** The operator forwards one message at a time, in the order that it receives them. Do not rely on a strict order, because a retry after an error can change it.

## Adapt this to your stack

To use queue splitting with a real consumer, change these items:

1. **Target.** In `MirrordSplitConfig`, set `spec.targetRef` to your workload. In `mirrord-up.yaml`, set `target.path` and `target.namespace` to the same workload and container.
2. **Queue name source.** Set `appConfig.queue[].env` to the environment variable that holds the queue name. If the name is part of a longer string, use `valuePattern`. If the variable holds JSON, use `valueSelector`. If the name is in a mounted config file, use a `volume` source.
3. **More than one queue.** Add one entry to `spec.queues` for each queue, each with its own `id`.
4. **Exchanges.** If your consumer reads an exchange name from the environment, add `appConfig.exchange`. The operator then gives your local consumer a dummy exchange name, so it does not publish to or bind against the real exchange.
5. **Broker connection.** In the `MirrordPropertyList`, set `host` or `url`, and the `username` and `password`. For TLS, set `scheme: amqps` and the certificate properties. For a vhost other than `/`, set `vhost`.
6. **Shared credentials.** To use one `MirrordPropertyList` in many namespaces, put it in the operator's namespace. A list with the same name in the target's namespace takes priority.
7. **Session key in the messages.** Make your producers send `mirrord-session=<key>` in a header. The W3C `baggage` header from your tracing library is a good choice. See [Header propagation](../../sharing-the-cluster/header-propagation.md).

The consumer code does not change. It reads the same environment variable, connects to the same broker, and consumes the same way.

To use your own filter instead of the session key, set `feature.split_queues` in the `config_patch` of the service. A `config_patch` that sets `split_queues` replaces all the queue splits that `mirrord up` makes for that service. This `mirrord-up.yaml` filters on a field in the JSON body. It needs mirrord CLI `3.253.0` or later:

```yaml
common:
  operator: true
services:
  orders-consumer:
    target:
      path: deployment/orders-consumer/container/main
      namespace: rmq-demo
    run:
      command: ["python3", "consumer.py", "{{ key }}"]
    config_patch:
      feature:
        split_queues:
          orders:
            queue_type: RMQ
            jq_filter: '.payload | fromjson | .user == "{{ key }}"'
```

The key `orders` is the queue ID from the `MirrordSplitConfig`. A message that does not have a valid JSON body never matches this filter.

## Cleanup

Stop all mirrord sessions with `Ctrl+C`. Then delete the demo namespace:

```bash
kubectl delete namespace rmq-demo
```

If you turned on RabbitMQ splitting only for this tutorial, turn it off:

```bash
helm upgrade mirrord-operator metalbear/mirrord-operator \
  --reuse-values \
  --set operator.rmqSplitting=false
```

Delete the local files:

```bash
rm consumer.py mirrord-up.yaml
rm -rf .venv
```

## Frequently asked questions

### Do I need to change my RabbitMQ consumer code to use queue splitting?

No. The operator changes the queue name in the environment variable or mounted file that your consumer already reads. Your consumer connects and consumes as usual. You do not add an SDK or a routing check.

### What happens when two developers' filters match the same RabbitMQ message?

Only one of the two sessions gets the message. The other session does not get it, and you cannot choose which session gets it. Give each developer a different session key, so that filters do not overlap. Do not use a key that is the start of a different key.

### Can I filter RabbitMQ messages on the message body instead of a header?

Yes. Set a `jq_filter` in the `config_patch` of the service, under `feature.split_queues`. The jq program sees the headers, the basic properties, and the body as `payload`. To read fields of a JSON body, start the program with `.payload | fromjson`. This needs operator `3.201.0` or later and CLI `3.253.0` or later.

### What happens to RabbitMQ messages when I stop my mirrord session?

If other sessions continue, new messages that matched your filter go to the deployed consumer again. The operator deletes your temporary queue, and unread messages in it are lost. When the last session ends, the operator waits until the deployed consumer reads all messages in its temporary queue, then removes the split. The `drainTimeout` of the `MirrordSplitConfig` sets a limit on this wait. To keep the split for a new session, set `ttl` on the `MirrordSplitConfig`.

### Does the deployed RabbitMQ consumer restart when I start a session?

Yes. The operator patches the workload so that it reads a temporary queue, and its pods restart. When the split ends, the operator restores the workload and the pods restart again.

### Can the deployed consumer still process the messages that my local consumer gets?

Yes. Start the session with `mirrord up --key alice --mode mirror`, or set `default_mode: mirror` on the service in `mirrord-up.yaml`. In `mirror` mode, a message that matches your filter goes to your local consumer and to the deployed consumer. The default mode is `split`, where a matched message goes only to your local consumer. The `mirror` mode of `mirrord up` needs CLI `3.258.0` or later and operator `3.183.0` or later.

## See also

- [RabbitMQ queue splitting reference](../../sharing-the-cluster/queue-splitting/rabbitmq.md)
- [Queue splitting overview](../../sharing-the-cluster/queue-splitting.md)
- [Setting a filter for a mirrord run](../../sharing-the-cluster/queue-splitting.md#setting-a-filter-for-a-mirrord-run)
- [Migrating to MirrordSplitConfig](../../sharing-the-cluster/queue-splitting/migrating-to-mirrordsplitconfig.md)
- [Header propagation](../../sharing-the-cluster/header-propagation.md)
