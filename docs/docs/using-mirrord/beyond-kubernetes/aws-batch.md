---
title: "AWS Batch"
description: "Add the mirrord remote bootstrap to an AWS Batch job definition, so developers can attach to the jobs whose tags match, or replace them with local processes."
tags:
  - alpha
  - enterprise
---

# AWS Batch

{% hint style="warning" %}
Targeting AWS Batch jobs is in **alpha**, and may change without notice. It works only in the [Operator-hosted](operator-hosted.md) deployment mode, and requires the Operator and the remote bootstrap image **3.239.0 or later**, and the mirrord CLI **3.292.0 or later**.
{% endhint %}

AWS Batch runs each job of a Fargate or EC2 compute environment as an Amazon ECS task, so a job is made targetable as a [workload outside Kubernetes](README.md) the same way an [ECS task](ecs.md) is. On top of that, developers can pick jobs by their tags:

* `mirrord exec` and the IDE extensions attach to a job whose tags match, while the job runs its own code.
* A `mirrord up` service starts one local process for every matching job, with that job's environment and arguments.
* With `replace_task`, a matching job doesn't run its own code. The local process runs in its place, and the job ends with the local process's exit code.

At a glance, the setup is:

1. Let the Operator read the jobs' tags ([Let the Operator read job tags](#let-the-operator-read-job-tags)).
2. Add the remote bootstrap to the job definition and name the workload ([Add the remote bootstrap](#add-the-remote-bootstrap)).
3. Connect the jobs to the Operator ([Connect the jobs to the Operator](#connect-the-jobs-to-the-operator)).
4. To let developers replace jobs, turn on admission in the job definition ([Replacing jobs](#replacing-jobs)).

## Prerequisites

1. The [Operator-hosted sessions-manager](operator-hosted.md) enabled on an EKS cluster, with an IAM role for the Operator's ServiceAccount, set with the `sa.roleArn` Helm value.
2. An AWS Batch compute environment on Fargate or EC2. Batch on EKS and multi-node parallel jobs aren't supported.
3. An application image based on glibc 2.30 or later, and not on musl, such as Alpine. See [Application container requirements](README.md#application-container-requirements).
4. A job role on the job definition: `jobRoleArn` with `containerProperties`, or `taskRoleArn` with `ecsProperties`.

Throughout this guide, replace:

| Placeholder | Value |
| --- | --- |
| `<YOUR_SERVICE_NAME>` | A name for the jobs you target, e.g. `report-generator` |
| `<YOUR_ENVIRONMENT>` | A name for the environment the jobs run in, e.g. `staging` |
| `<YOUR_APPLICATION_IMAGE>` | Your job's existing image |
| `<JOB_ROLE_ARN>` | The ARN of the job definition's job role |
| `<OPERATOR_ROLE_NAME>` | The name of the IAM role in the Operator's `sa.roleArn` |

---

## Let the Operator read job tags

The Operator reads each job's tags itself, with `DescribeJobs`, when the job's task registers. Attach this statement to the Operator's IAM role:

```bash
aws iam put-role-policy --role-name <OPERATOR_ROLE_NAME> \
  --policy-name mirrord-batch-job-tags \
  --policy-document '{
    "Version": "2012-10-17",
    "Statement": [
      { "Effect": "Allow", "Action": "batch:DescribeJobs", "Resource": "*" }
    ]
  }'
```

`DescribeJobs` has no resource-level permissions, so the statement lets the Operator read every job in the account, including each job's command and environment variables. To limit it to the regions your jobs run in, add a condition on `aws:RequestedRegion`. The job's role needs no extra permission.

Without the statement, the Operator logs a warning, jobs have no tags, and no tag filter matches them.

## Add the remote bootstrap

A Batch job definition has one of two forms. Use the section for yours.

### With `ecsProperties`

A job definition with `ecsProperties` holds several containers, so a setup container copies the bootstrap into a shared volume, as in [Amazon ECS](ecs.md#add-the-remote-bootstrap). Merge the example into your existing job definition, keeping its other settings:

```json
{
  "ecsProperties": {
    "taskProperties": [
      {
        "taskRoleArn": "<JOB_ROLE_ARN>",
        "volumes": [
          { "name": "mirrord" }
        ],
        "containers": [
          {
            "name": "install-mirrord-remote-bootstrap",
            "image": "ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.239.0",
            "essential": false,
            "entryPoint": ["/bin/sh", "-c"],
            "command": [
              "cp /opt/mirrord/lib/libmirrord_remote_bootstrap.so /mirrord/libmirrord_remote_bootstrap.so"
            ],
            "mountPoints": [
              { "sourceVolume": "mirrord", "containerPath": "/mirrord" }
            ]
          },
          {
            "name": "app",
            "image": "<YOUR_APPLICATION_IMAGE>",
            "essential": true,
            "dependsOn": [
              { "containerName": "install-mirrord-remote-bootstrap", "condition": "SUCCESS" }
            ],
            "mountPoints": [
              { "sourceVolume": "mirrord", "containerPath": "/opt/mirrord/lib", "readOnly": true }
            ],
            "environment": [
              { "name": "LD_PRELOAD", "value": "/opt/mirrord/lib/libmirrord_remote_bootstrap.so" },
              { "name": "MIRRORD_REMOTE_SERVICE", "value": "<YOUR_SERVICE_NAME>" },
              { "name": "MIRRORD_REMOTE_ENVIRONMENT", "value": "<YOUR_ENVIRONMENT>" }
            ]
          }
        ]
      }
    ]
  }
}
```

### With `containerProperties`

A job definition with `containerProperties` holds a single container, so bake the bootstrap into the application image:

```dockerfile
FROM <YOUR_APPLICATION_IMAGE>
COPY --from=ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.239.0 \
  /opt/mirrord/lib/libmirrord_remote_bootstrap.so /opt/mirrord/lib/libmirrord_remote_bootstrap.so
```

and add the variables to the container's `environment`:

```json
{
  "containerProperties": {
    "image": "<YOUR_IMAGE_WITH_THE_BOOTSTRAP>",
    "jobRoleArn": "<JOB_ROLE_ARN>",
    "environment": [
      { "name": "LD_PRELOAD", "value": "/opt/mirrord/lib/libmirrord_remote_bootstrap.so" },
      { "name": "MIRRORD_REMOTE_SERVICE", "value": "<YOUR_SERVICE_NAME>" },
      { "name": "MIRRORD_REMOTE_ENVIRONMENT", "value": "<YOUR_ENVIRONMENT>" }
    ]
  }
}
```

Without `LD_PRELOAD` in the job definition, the image runs without mirrord.

### Name the workload

| Variable | Value |
| --- | --- |
| `LD_PRELOAD` | The path of the bootstrap library, as above. |
| `MIRRORD_REMOTE_SERVICE` | `<YOUR_SERVICE_NAME>`. Developers target the jobs as `serverless/<YOUR_SERVICE_NAME>`. |
| `MIRRORD_REMOTE_ENVIRONMENT` | `<YOUR_ENVIRONMENT>`. Developers set it as `target.namespace`. |

Every job of the job definition registers under that service, whichever queue it was submitted to. The bootstrap also sends the job's ID, which Batch sets in `AWS_BATCH_JOB_ID`, so that the Operator can read the job's tags.

## Connect the jobs to the Operator

A job authenticates as its job role, the way an ECS task authenticates as its task role. Follow [Connecting ECS to the Operator](ecs-operator-hosted.md) with the job role in place of the task role:

1. [Grant the ECS task access](ecs-operator-hosted.md#grant-the-ecs-task-access), once per cluster.
2. [Map the job role into the cluster](ecs-operator-hosted.md#map-the-ecs-task-role-into-the-cluster), with `--principal-arn <JOB_ROLE_ARN>`.
3. [Make the EKS API server reachable](ecs-operator-hosted.md#make-the-eks-api-server-reachable-from-ecs) from the compute environment's subnets.
4. [Add the connection variables](ecs-operator-hosted.md#add-the-connection-variables) to the application container's `environment`, and register the new job definition revision.

---

## Selecting jobs by their tags

A serverless target selects jobs by their tags, as `DescribeJobs` returns them, with two settings:

* `target.tag_filter`: a mapping of tag names to regexes. A job matches when it has every named tag, each with a value its regex matches. A regex matches anywhere in the value, so anchor it with `^` and `$` to match the whole value.
* `target.jq_filter`: a jq program run on `{"tags": {"<name>": "<value>", ...}}`. A job matches when the program outputs `true`.

With both set, a job must match both. Both need `operator: true`. An empty `tag_filter` without a `jq_filter` is refused, because it would match every job.

### Attaching to a job

With `mirrord exec` or an IDE extension, set the filter in `mirrord.json`:

```json
{
  "operator": true,
  "target": {
    "path": "serverless/<YOUR_SERVICE_NAME>",
    "namespace": "<YOUR_ENVIRONMENT>",
    "tag_filter": { "company_id": "^1234$" }
  }
}
```

mirrord waits for a matching job when none runs yet. Among several, it attaches to one. The whole session stays on that job, reconnects included, and ends when the job ends. The job runs its own code. See [`target.tag_filter`](https://metalbear.com/mirrord/docs/config/options#target-tag_filter) and [`target.jq_filter`](https://metalbear.com/mirrord/docs/config/options#target-jq_filter).

### A local process for every matching job

In `mirrord-up.yaml`, a service with a filter gets one local process for every matching job:

```yaml
common:
  operator: true
services:
  reports:
    target:
      path: serverless/<YOUR_SERVICE_NAME>
      namespace: <YOUR_ENVIRONMENT>
      tag_filter:
        company_id: "^1234$"
    run:
      command: ["python", "generate.py"]
```

Each local process starts with its job's environment, and with the job's arguments appended to `run.command`, and is stopped when its job ends. The jobs still run their own code. See [Tasks selected by tags](../multiple-concurrent-sessions.md#tasks-selected-by-tags).

## Replacing jobs

With `replace_task: true` on the service, a matching job doesn't run its own code: the local process started for it runs in its place, and the job ends with the local process's exit code.

```yaml
common:
  operator: true
services:
  reports:
    target:
      path: serverless/<YOUR_SERVICE_NAME>
      namespace: <YOUR_ENVIRONMENT>
      tag_filter:
        company_id: "^1234$"
    replace_task: true
    run:
      command: ["python", "generate.py"]
```

Only jobs that turn on admission are replaced. Add it to the application container's `environment`:

```json
{ "name": "MIRRORD_REMOTE_ADMISSION", "value": "1" }
```

| Variable | Value |
| --- | --- |
| `MIRRORD_REMOTE_ADMISSION` | Any value turns admission on. Without it, the job starts as it does without mirrord's admission, with no delay, and is never replaced. |
| `MIRRORD_REMOTE_ADMISSION_TIMEOUT` | How long a job waits for the Operator's answer, in whole seconds. Defaults to `5`. With `0` or `1`, the job is never replaced, though with `1` it can still wait up to a second for mirrord to connect to it. |

With admission on, the bootstrap holds the job's application before it starts, and asks the Operator whether a developer replaces the job. A job that no `replace_task` service matches starts its application as soon as the answer arrives, usually within a second. A job that one matches is held until its local process exits.

{% hint style="warning" %}
When the Operator is unreachable, or older than 3.239.0, no answer comes, and every job with admission on waits almost the full `MIRRORD_REMOTE_ADMISSION_TIMEOUT` before it starts its application: mirrord gives up on the answer a second before the timeout. Turn admission on only for jobs used in development, and only with a supported Operator.
{% endhint %}

A `replace_task` service replaces only the jobs that start while `mirrord up` runs. Jobs that already run aren't affected. When the filters of several developers match a job, the developer whose `mirrord up` started first gets it.

### Exit codes

| Local process | The job's exit code |
| --- | --- |
| Exits with a code | That code. A code outside `0` to `255` becomes `1`. |
| Killed by a signal | `128` plus the signal's number, e.g. `137` for `SIGKILL`. `137` can also mean that mirrord ended the local process. |
| mirrord fails before it starts the local process | `214` |
| The run ends while the job is held, for any reason: another service exits, a watch fails, or you press Ctrl-C | `214` |
| Session lost: `mirrord up` is killed, or no connection from the developer reaches the job for 30 seconds | `214` |

To submit a lost job again, retry on `214` in the job definition's retry strategy. Each attempt goes through admission again:

```json
{
  "retryStrategy": {
    "attempts": 2,
    "evaluateOnExit": [
      { "onExitCode": "214", "action": "RETRY" },
      { "onExitCode": "*", "action": "EXIT" }
    ]
  }
}
```

A replaced job counts the time its local process runs against its attempt duration, so give jobs used in development a long `timeout`, such as `{ "attemptDurationSeconds": 86400 }`. `aws batch terminate-job` stops a replaced job, and its local process with it.

---

## Limitations

* **Operator-hosted mode only.** mirrord refuses `tag_filter`, `jq_filter` and `replace_task` without `operator: true`, and with MetalBear Cloud.
* **Replacing is `mirrord up` only.** `mirrord exec` and the IDE extensions attach to a job without replacing it.
* **No limit on matched jobs.** A `mirrord up` service starts a local process for every job its filters match.
* **No reason for a miss.** When no job matches, mirrord says that it waits, and the Operator logs a warning when it can't read a job's tags.
* **Operator restarts.** A replaced job survives an Operator restart only if mirrord connects to it again within 30 seconds, and ends with `214` otherwise. `mirrord up` registers again on its own.
* **No `--debug`.** `mirrord up --debug` can't name a service with a filter.
* **Not shown in `mirrord ui`.** The matched jobs aren't listed there.
* All the [limitations of workloads outside Kubernetes](README.md#limitations) also apply.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| mirrord waits, and no job matches | The job's tags don't match the filters, the job's bootstrap is older than 3.239.0, or the Operator's role lacks `batch:DescribeJobs` ([Let the Operator read job tags](#let-the-operator-read-job-tags)). |
| A matched job runs its own code with `replace_task` | The job definition doesn't set `MIRRORD_REMOTE_ADMISSION`, or the job started before `mirrord up`. |
| Every job waits 5 seconds before it starts | Admission is on, but the job can't reach the Operator, or the Operator is older than 3.239.0. |
| A replaced job ends with `214` | The session was lost before the local process exited ([Exit codes](#exit-codes)). |

For connection and authentication problems, see [Connecting ECS to the Operator](ecs-operator-hosted.md#troubleshooting).
