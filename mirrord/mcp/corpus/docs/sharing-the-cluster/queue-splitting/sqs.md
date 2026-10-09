---
title: Amazon SQS
tags:
  - beta
  - team
  - enterprise
---

This page covers queue splitting for [Amazon SQS](https://aws.amazon.com/sqs/). For the general concepts and the message filter reference shared by all queue services, see the [Queue Splitting overview](../queue-splitting.md).

{% hint style="info" %}
Queue splitting via `MirrordSplitConfig` requires mirrord operator `3.170.0` or later and mirrord CLI `3.221.0` or later.
{% endhint %}

{% hint style="warning" %}
**⚠️ Deprecated CRD**

`MirrordWorkloadQueueRegistry` is deprecated and replaced by `MirrordSplitConfig`. Existing resources continue to work for backward compatibility, but we recommend migrating to `MirrordSplitConfig`. See [Migrating to MirrordSplitConfig](migrating-to-mirrordsplitconfig.md#amazon-sqs).

The older `operator.sqsSplittingLingerTimeout` Helm value only affects legacy `MirrordWorkloadQueueRegistry`; with `MirrordSplitConfig`, use [`spec.drainTimeout`](sqs.md#if-all-sqs-sessions-are-over-but-the-remote-service-still-didnt-change-back-to-read-from-the-original-queue) instead.
{% endhint %}

## How It Works

First, we have a consumer app reading messages from an SQS queue:

![A K8s application that consumes messages from an SQS queue](../../.gitbook/assets/before-splitting-sqs.svg)

When the first mirrord SQS splitting session starts, two temporary queues are created (one for the target deployed in the cluster, one for the user's local application), and the mirrord operator routes messages according to the [user's filter](sqs.md#setting-a-filter):

![One SQS splitting session](../../.gitbook/assets/1-user-sqs.svg)

If a second user then starts a mirrord SQS splitting session on the same queue, a third temporary queue is created (for the second user's local application). The mirrord operator includes the new queue and the second user's filter in the routing logic.

![Two SQS splitting sessions](../../.gitbook/assets/2-users-sqs.svg)

If the filters defined by the two users both match some message, one of the users will receive the message at random.

Deployed targets will keep reading from the temporary queues as long as their temporary queues have unconsumed messages, so no messages intended for the remote service are lost.

## Enabling SQS Splitting in Your Cluster

{% stepper %}
{% step %}
#### Enable SQS splitting in the Helm chart

Enable the `operator.sqsSplitting` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml).
{% endstep %}

{% step %}
#### Authenticate and authorize the mirrord operator

The mirrord operator will need to be able to perform operations on the SQS queues. To do this, it will build an SQS client, using the default credentials provider chain.

The easiest way to provide the credentials for the operator is with IAM role assumption. For that, an IAM role with an appropriate policy has to be assigned to the operator's service account. Please follow [AWS's documentation on how to do that](https://docs.aws.amazon.com/eks/latest/userguide/associate-service-account-role.html). Note that operator's service account can be annotated with the IAM role's ARN with the `sa.roleArn` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml).

Some of the SQS permissions are needed for your actual queues that you would like to split, and some permissions are only needed for the temporary queues, managed by the operator. Here is an overview:

| SQS Permission     | needed for your queues | needed for temporary queues |
| ------------------ | :--------------------: | :-------------------------: |
| GetQueueUrl        |            ✓           |                             |
| ListQueueTags      |            ✓           |                             |
| ReceiveMessage     |            ✓           |                             |
| DeleteMessage      |            ✓           |                             |
| GetQueueAttributes |            ✓           |          ✓ (both!)          |
| CreateQueue        |                        |              ✓              |
| TagQueue           |                        |              ✓              |
| SendMessage        |                        |              ✓              |
| DeleteQueue        |                        |              ✓              |

And here we provide a short explanation for each required permission:

* `sqs:GetQueueUrl`: the operator finds queue names to split in the provided source, and then it fetches the URL from SQS in order to make other API calls.
* `sqs:GetQueueAttributes`: the operator queries your queue's attributes, in order to clone these attributes to all derived temporary queues. It also reads the attributes of the temporary queues, in order to check the number of remaining messages.
* `sqs:ListQueueTags`: the operator queries your queue's tags, in order to clone these tags to all derived temporary queues.
* `sqs:ReceiveMessage`: the operator reads messages from the queues you split.
* `sqs:DeleteMessage`: after reading a message and forwarding it to a temporary queue, the operator deletes the message from the split queue.
* `sqs:CreateQueue`: the operator creates temporary queues in your SQS account.
* `sqs:TagQueue`: the operator sets tags on the temporary queues.
* `sqs:SendMessage`: the operator sends messages to the temporary queues.
* `sqs:DeleteQueue`: the operator deletes stale temporary queues in the background.

This is an example for a policy that gives the operator's roles the minimal permissions it needs to split a queue called `ClientUploads`:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "sqs:GetQueueUrl",
        "sqs:GetQueueAttributes",
        "sqs:ListQueueTags",
        "sqs:ReceiveMessage",
        "sqs:DeleteMessage"
      ],
      "Resource": [
        "arn:aws:sqs:eu-north-1:314159265359:ClientUploads"
      ]
    },
    {
      "Effect": "Allow",
      "Action": [
        "sqs:CreateQueue",
        "sqs:TagQueue",
        "sqs:SendMessage",
        "sqs:GetQueueAttributes",
        "sqs:DeleteQueue"
      ],
      "Resource": "arn:aws:sqs:eu-north-1:314159265359:mirrord-*"
    }
  ]
}
```

*   The first statement gives the role the permissions it needs for your original queues.

    Instead of specifying the queues you would like to be able to split in the first statement, you could alternatively make that statement apply for all resources in the account, and limit the queues it applies to using conditions instead of resource names. For example, you could add a condition that makes the statement only apply to queues with the tag `splittable=true` or `env=dev` etc. and set those tags for all queues you would like to allow the operator to split.
*   The second statement in the example gives the role the permissions it needs for the temporary queues. Since all the temporary queues created by mirrord are created with the name prefix `mirrord-`, that statement in the example is limited to resources with that prefix in their name.

    If you would like to limit the second statement with conditions instead of (only) with the resource name, you can [set a condition that requires a tag](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-abac-tagging-resource-control.html), and for each queue you can specify tags that mirrord will set on the temporary queues it creates for that original queue (see [per-queue options](sqs.md#per-queue-options-sns-s3-events-tags)).

If the queue messages are encrypted, the operator's IAM role should also have the following permissions:

* `kms:Encrypt`
* `kms:Decrypt`
* `kms:GenerateDataKey`

If you enable `s3_event` for an SQS queue (via its `queueConfig`), the operator will also fetch S3 object metadata for jq filtering. In that case, grant the operator:

* `s3:GetObject` on the relevant buckets / object prefixes.
* `s3:ListBucket` on the relevant buckets if you want S3 to return `404 Not Found` for deleted or missing objects instead of `403 Forbidden`.
{% endstep %}

{% step %}
#### Authorize deployed consumers

In order to be targeted with SQS splitting, a deployed consumer must be able to use the temporary queues created by mirrord. E.g. if the consumer application retrieves the queue's URL based on its name, lists queue's tags, consumes and deletes messages from the queue — it must be able to do the same on a temporary queue.

Any temporary queues managed by mirrord are created with the same policy as the original queues they are splitting (with the single change of updating the queue name in the policy). Therefore, access control based on SQS policies should automatically be taken care of.

However, if the consumer's access to the queue is controlled by an IAM policy (and not an SQS policy, see [SQS docs](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-using-identity-based-policies.html#sqs-using-sqs-and-iam-policies)), you will need to adjust it.
{% endstep %}

{% step %}
#### Provide application context

On operator installation with `operator.sqsSplitting` enabled, a new [`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) type is defined in your cluster - `MirrordSplitConfig`. Users with permissions to get CRDs can verify its existence with `kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co`. Before you can run sessions with SQS splitting, you must create a `MirrordSplitConfig` for the desired target. This is because the `MirrordSplitConfig` contains additional application context required by the mirrord operator. For example, the operator needs to know which environment variables contain the names of the SQS queues to split.

See an example `MirrordSplitConfig` defined for a deployment `meme-app` living in namespace `meme`:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: meme-app-split
  namespace: meme
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: meme-app
  queues:
    - id: meme-queue
      kind: sqs
      # Optional per-queue options (SNS parsing, tags). See below.
      queueConfig: meme-queue-options
      appConfig:
        queue:
          - env: INCOMING_MEME_QUEUE_NAME
            containers:
              - main
    - id: ad-queue
      kind: sqs
      appConfig:
        queue:
          - env: AD_QUEUE_NAME
            containers:
              - main
```

The `MirrordSplitConfig` above says that:

1. It targets the deployment `meme-app` in namespace `meme`, restricted to container `main`.
2. The container consumes two SQS queues. Their names are read from environment variables `INCOMING_MEME_QUEUE_NAME` and `AD_QUEUE_NAME`.
3. The SQS queues can be referenced in a mirrord config under IDs `meme-queue` and `ad-queue`, respectively.
4. The `meme-queue` has extra per-queue options defined in the `meme-queue-options` `MirrordPropertyList` (e.g. tags set on temporary queues, SNS parsing).

#### Link the config to the deployed consumer

The `MirrordSplitConfig` is a namespaced resource, so it can only reference a consumer deployed in the same namespace. The target workload reference is specified with `spec.targetRef`:

* `apiVersion` - API version of the Kubernetes workload (e.g. `apps/v1`).
* `kind` - type of the workload. mirrord supports SQS splitting on deployments and Argo rollouts.
* `name` - name of the workload.

#### Describe consumed queues

Each entry in the `spec.queues` list describes one or more SQS queues consumed by the workload:

* `id` - arbitrary queue ID, as it will only be [referenced](sqs.md#setting-a-filter) from the user's mirrord config.
* `kind` - must be `sqs`.
* `appConfig.queue` - how the application discovers the queue name/URL. Each entry can use:
  * `env` - exact name of a single environment variable containing the queue name or URL.
  * `envLike` - regex selecting multiple environment variables by name.
  * `volume` - read the queue name from a file mounted from a `configMap` volume (`volume.name` + `volume.file`) instead of an environment variable. See [Queue Names in Mounted Config Files](../queue-splitting.md#queue-names-in-mounted-config-files).
  * `fallback` - fallback name/URL if the variable is not found (only valid with `env`). The temporary queue name is still injected into the variable.
  * `valueSelector` - a selector extracting queue names from the variable's value: nested keys (`.queues.orders`) and `.[]` to iterate arrays or object values. Use `.[]` when the variable holds a JSON object whose string values are queue names/URLs. Pipes, functions, and other jq operators are not supported.
  * `valuePattern` - a regex used when the queue name/URL is embedded in a larger string. The capture group (named `value`, otherwise the first group) marks the part that is the name; only that part is swapped for the temporary queue and the surrounding text is kept as-is.
  * `containers` - limit resolution to specific containers (optional, defaults to all containers).
* `queueConfig` (optional) - name of a `MirrordPropertyList` holding per-queue options (see below).

#### Per-queue options (SNS, S3 events, tags)

SQS-specific options live in a `MirrordPropertyList` referenced by the queue's `queueConfig`. The property list is looked up in the namespace of the `MirrordSplitConfig`, then in the operator's namespace - see [Sharing Property Lists Across Namespaces](../queue-splitting.md#sharing-property-lists-across-namespaces). Supported properties:

* `sns` (`"true"`/`"false"`) - set to `"true"` if the queue contains SQS messages created from SNS notifications. Message bodies are then parsed and matched against users' filters, since SNS notification attributes live in the SQS message body. Defaults to `"false"`.
* `s3_event` (`"true"`/`"false"`) - set to `"true"` to parse incoming messages as S3 event notifications and, on success, fetch user-defined S3 object metadata for the referenced object and expose it to `jq_filter` as `S3Metadata`.
  * **S3 to SQS** (direct): set only `s3_event: "true"`.
  * **S3 to SNS to SQS**: set both `sns: "true"` and `s3_event: "true"`. Defaults to `"false"`.
* `tags` - a JSON object string of tags that mirrord sets on every temporary queue it creates for this queue, e.g. `'{"tool":"mirrord"}'`.

For example, a `MirrordPropertyList` for a queue that tags its temporary queues and receives S3 events delivered through SNS:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: meme-queue-options
  namespace: meme
spec:
  properties:
    - name: sns
      value: 'true'
    - name: s3_event
      value: 'true'
    - name: tags
      value: '{"tool":"mirrord"}'
```

{% hint style="warning" %}
The mirrord operator can only read consumer's environment variables if they are either:

1. defined directly in the workload's pod template, with the value defined in `value` or in `valueFrom` via config map reference; or
2. loaded from config maps using `envFrom`.
{% endhint %}
{% endstep %}
{% endstepper %}

## Setting a filter

For the full filter reference (`queue_type`, `filter`, `message_filter`, `jq_filter`), see the [overview](../queue-splitting.md#setting-a-filter-for-a-mirrord-run). SQS uses `queue_type: SQS`. SQS attribute names and values are matched case-insensitively.

The same session filter with the composable `filter` shape, which can also combine several attributes with `any_of` / `all_of`:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "meme-queue": {
        "queue_type": "SQS",
        "filter": {
          "any_of": [
            { "metadata": "^baggage: .*mirrord-session=alice.*$" },
            { "metadata": "^tracestate: .*mirrord-session=alice.*$" }
          ]
        }
      }
    }
  }
}
```


Filtering on SQS message attributes, an unmatched (match-none) queue, and a `jq_filter` on the message body:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "meme-queue": {
        "queue_type": "SQS",
        "message_filter": {
          "baggage": ".*mirrord-session=alice.*"
        }
      },
      "orders-queue": {
        "queue_type": "SQS",
        "jq_filter": ".Body | fromjson | .important == true"
      },
      "ad-queue": {
        "queue_type": "SQS",
        "message_filter": {}
      }
    }
  }
}
```

In the example above, the local application:

* Will receive a subset of messages from SQS queues described in the `MirrordSplitConfig` under ID `meme-queue`. All received messages will have an SQS attribute `baggage` containing `mirrord-session=alice`.
* Will receive a subset of messages from SQS queues described in the `MirrordSplitConfig` under ID `orders-queue`. All received messages will have a JSON body with `"important": true`.
* Will receive no messages from SQS queues described in the `MirrordSplitConfig` under ID `ad-queue`.

A `jq_filter` on the message body or on a message attribute:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "orders-queue": {
        "queue_type": "SQS",
        "jq_filter": ".Body | fromjson | .client == \"a\""
      },
      "fifo-orders-queue": {
        "queue_type": "SQS",
        "jq_filter": ".MessageAttributes.client.StringValue | test(\"^a$\")"
      }
    }
  }
}
```

In the example above, the local application:

* Will receive messages from SQS queue `orders-queue` only when the message body is valid JSON and contains `"client": "a"`.
* Will receive messages from SQS queue `fifo-orders-queue` only when the SQS message attribute `client` has the value `a`.

Filtering on S3 object metadata (requires `s3_event: "true"` on the queue's `queueConfig`):

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "uploads-queue": {
        "queue_type": "SQS",
        "jq_filter": ".S3Metadata.client == \"a\""
      }
    }
  }
}
```

In the example above, the local application will receive messages from SQS queue `uploads-queue` only when the S3 object referenced by the event has `S3Metadata.client == "a"`.

From mirrord operator `3.215.0`, only jq filters that name `S3Metadata` make the operator fetch the metadata, and at most once per message. Sessions that filter on attributes or on other message fields keep working even when S3 cannot be reached. The check is a plain text match: the program must contain `S3Metadata` literally, so a program that reaches the field another way (a computed key such as `.[$name]`, or `to_entries`) sees no `S3Metadata`.

Combining an attribute filter with a `jq_filter` (both must match):

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "orders-queue": {
        "queue_type": "SQS",
        "message_filter": {
          "client": "^a$"
        },
        "jq_filter": ".Body | fromjson | .important == true"
      }
    }
  }
}
```

In the example above, the local application will receive messages from SQS queue `orders-queue` only when both of these conditions hold:

* The SQS message attribute `client` has the value `a`.
* The JSON message body contains `"important": true`.

Using the `*` wildcard to apply one filter to all queues described in the `MirrordSplitConfig`:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "*": {
        "queue_type": "SQS",
        "message_filter": {
          "baggage": ".*mirrord-session=pr-123.*"
        }
      }
    }
  }
}
```

In the example above, the local application will receive a subset of message from **all** SQS queues described in the `MirrordSplitConfig`. All received messages will have an SQS attribute `baggage` containing `mirrord-session=pr-123`. `*` is a special queue ID for SQS queues, and resolves to all queues described in the `MirrordSplitConfig`.

## Troubleshooting SQS splitting

If you're trying to use SQS-splitting and are facing difficulties, here are some steps you can go through to identify and hopefully solve the problem.

First, some generally applicable steps:

1.  Make sure a `MirrordSplitConfig` exists for the workload you're targeting:

    ```shell
    kubectl describe mirrordsplitconfigs.queues.mirrord.metalbear.co -n <target-namespace>
    ```
2. Note [the queue-ids in the mirrord configuration](sqs.md#setting-a-filter) have to match the queue-ids in the `MirrordSplitConfig` of the used target.
3.  Get the logs from the mirrord-operator, in case it becomes necessary for the mirrord team to look into your issue, like this:

    ```shell
    kubectl logs -n mirrord -l app==mirrord-operator --tail -1 > /tmp/mirrord-operator-$(date +"%Y-%m-%d_%H-%M-%S").log
    ```

    If that fails, you might not have permissions to the operator's logs.

    To get especially helpful logs, you can change the log level for SQS-splitting, then reproduce the issue to get the relevant logs. This can be achieved by reinstalling the helm chart and setting the `operator.logLevel` helm value to `mirrord=info,operator=info,operator_sqs_splitting::forwarder=trace`:

    ```shell
    helm upgrade mirrord-operator --reuse-values --set operator.logLevel "mirrord=info,operator=info,operator_sqs_splitting::forwarder=trace" metalbear/mirrord-operator
    ```

    or by setting the `RUST_LOG` environment variable in the operator's deployment to `mirrord=info,operator=info,operator_sqs_splitting::forwarder=trace`, e.g. by using `kubectl edit deploy mirrord-operator -n mirrord`.
4. Some operations, like changing a `MirrordSplitConfig` of a workload while there are active sessions to that target, are not yet supported, and can lead to a bad state for mirrord. If you've reached such a state:
   1.  Delete all the mirrord split session resources of the affected target. Those resources, `MirrordClusterSplitSession`, are not the same as `MirrordSplitConfig`. They are cluster-scoped, and reference the target by namespace and workload name. You can delete the ones for your target with:

       ```shell
       kubectl get mirrordclustersplitsessions.queues.mirrord.metalbear.co -o json \
       | jq -r '.items[] | select(.spec.namespace=="<TARGET-NAMESPACE>" and .spec.target.name=="<TARGET-WORKLOAD-NAME>") | .metadata.name' \
       | xargs -r -I {} kubectl delete mirrordclustersplitsessions.queues.mirrord.metalbear.co {}
       ```
   2.  Restart the mirrord operator, e.g.:

       ```shell
       kubectl rollout restart deployment mirrord-operator -n mirrord
       ```

### If some (but not all) of the messages that should arrive at the local service arrive at the remote service

It's possible the target workload's restart is not complete yet, and there are still pods reading directly from the original queue (those will be pods that DO NOT have a `operator.metalbear.co/patched` label). You can wait a bit for them to be replaced with new pods, patched by mirrord, that read from a temporary queue created by mirrord, or you can delete them.

### If all SQS sessions are over but the remote service still didn't change back to read from the original queue

When there are no more queue splitting sessions to a target, the target workload will not immediately be changed to read directly from the original queue. Instead, it will keep reading from the temporary queue until its empty, so that no messages intended for the remote service are lost.

If the target workload doesn't change back within the expected time, check its logs and make sure it is consuming queue messages.

If you don't want to wait for the remote service to drain the temporary queue, and you don't care about losing those messages, you can cap the wait with `spec.drainTimeout` (in seconds) on the `MirrordSplitConfig`:

| `spec.drainTimeout` | Behavior                                                                              |
| ------------------- | ------------------------------------------------------------------------------------- |
| unset               | Drain indefinitely - the temporary queue is kept until the remote service empties it. |
| `0`                 | Skip draining; delete the temporary queue immediately. Unread messages may be lost.   |
| `N`                 | Wait up to `N` seconds for the queue to drain, then delete it.                        |

If that service is trying to consume messages correctly, and the temporary queue is already empty, but the target application still doesn't get restored to its original state, please try restarting the application, deleting any lingering `MirrordClusterSplitSession` objects, and if possible, restart the mirrord operator.
