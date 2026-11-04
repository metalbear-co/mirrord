---
title: "Amazon ECS"
description: "Add the mirrord remote bootstrap to an Amazon ECS task definition, so developers can target the service's tasks with mirrord."
tags:
  - experimental
  - enterprise
---

# Amazon ECS

{% hint style="warning" %}
Targeting Amazon ECS tasks is **experimental**, and may change without notice. See [Required versions](README.md#required-versions) for the bootstrap image and CLI versions.
{% endhint %}

This guide makes an Amazon ECS service targetable as a [serverless workload](README.md). The mirrord remote bootstrap is loaded into your existing application container with `LD_PRELOAD`; it doesn't need to be baked into your image, and your application image is unchanged.

The task-definition changes apply to **Linux tasks on both AWS Fargate and Amazon EC2**. Windows containers aren't covered by this setup. At a glance, the setup is:

1. Add a setup container that copies the bootstrap into a shared task volume ([Add the remote bootstrap](#add-the-remote-bootstrap)).
2. Load the bootstrap into the application container and name the workload ([Name the workload](#name-the-workload)).
3. Connect the task to your [deployment mode](README.md#deployment-modes), which adds its connection variables to the task and deploys it ([Connect to your deployment mode](#connect-to-your-deployment-mode)).

## Prerequisites

1. A [deployment mode](README.md#deployment-modes) that's set up: the [Operator-hosted sessions-manager](operator-hosted.md) or [MetalBear Cloud](metalbear-cloud.md).
2. Permission to register ECS task definitions and update the ECS service.
3. A writable `/tmp` in the application container. If the task definition sets `readonlyRootFilesystem`, mount a task volume at `/tmp`.
4. An application image based on glibc 2.30 or later, and not on musl, such as Alpine. See [Application container requirements](README.md#application-container-requirements).

The setup uses a `SUCCESS` [container dependency](https://docs.aws.amazon.com/AmazonECS/latest/APIReference/API_ContainerDependency.html), which requires:

| Launch type | Requirement |
| --- | --- |
| Fargate | Linux platform version **1.3.0 or later** |
| EC2 | ECS container agent **1.26.0 or later**; on ECS-optimized AMIs, `ecs-init` **1.26.0-1 or later** |

For EC2-backed tasks, use an up-to-date ECS-optimized AMI or ensure your custom AMI meets these requirements. Task-role credential requirements for the Operator-hosted mode are covered in [Connecting ECS to the Operator](ecs-operator-hosted.md#how-ecs-tasks-authenticate).

Throughout this guide, replace:

| Placeholder | Value |
| --- | --- |
| `<YOUR_SERVICE_NAME>` | A name for the ECS service you target, e.g. `payments` |
| `<YOUR_ENVIRONMENT>` | A name for the environment the ECS service runs in, e.g. `staging` |
| `<YOUR_APPLICATION_IMAGE>` | Your application container's existing image |

`<YOUR_SERVICE_NAME>` and `<YOUR_ENVIRONMENT>` are free-form: they're how a developer names the ECS task they want. They must be identical on the ECS task and in developers' `mirrord.json`.

---

## Add the remote bootstrap

A non-essential setup container copies the bootstrap library into a shared task volume before the application container starts, the ECS equivalent of a Kubernetes init container.

For EC2-backed tasks without task-level `memory`, also set a non-zero `memory` or `memoryReservation` on the setup container, as ECS requires a memory setting for each container in that case.

Merge the example into your existing task definition, preserving its launch type, network mode, resource settings and other containers. Add the volume and the setup container, and make the application container depend on it and mount the volume:

```json
{
  "volumes": [
    { "name": "mirrord" }
  ],
  "containerDefinitions": [
    {
      "name": "install-mirrord-remote-bootstrap",
      "image": "ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.229.0",
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
```

The example uses the public `ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.229.0` image. Bootstrap tags follow Operator release versions, not CLI versions; pin a release tag rather than `latest`. See [Required versions](README.md#required-versions) for the Operator and CLI requirements. If the ECS tasks can't pull from the image's registry, copy the image to a registry they can pull from, such as Amazon ECR. Because the setup container is non-essential with a `SUCCESS` dependency, a failed copy prevents the application container from starting, so you never silently run without mirrord.

## Name the workload

The application container's `environment` loads the bootstrap and names the workload:

| Variable | Value |
| --- | --- |
| `LD_PRELOAD` | The path of the bootstrap library in the shared volume, as above. |
| `MIRRORD_REMOTE_SERVICE` | `<YOUR_SERVICE_NAME>`. Developers target it as `serverless/<YOUR_SERVICE_NAME>`. |
| `MIRRORD_REMOTE_ENVIRONMENT` | `<YOUR_ENVIRONMENT>`. Developers set it as `target.namespace`. |

## Connect to your deployment mode

The bootstrap needs to know which sessions-manager to register with, and how to authenticate to it. Follow the guide for your deployment mode. It adds the connection variables to the same `environment` list, and finishes by deploying the new task definition revision:

| Deployment mode | Guide |
| --- | --- |
| Operator-hosted | [Connecting ECS to the Operator](ecs-operator-hosted.md) |
| MetalBear Cloud | [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md) |

---

## Sharing settings across services

ECS has no cluster-wide environment variables, so every task definition you target needs its variables. To keep the values that are the same for every service in one place, such as `MIRRORD_REMOTE_ENVIRONMENT` and your deployment mode's non-secret connection variables, put them in an [environment file](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/use-environment-file.html) in S3:

```bash
cat > mirrord.env <<EOF
MIRRORD_REMOTE_ENVIRONMENT=<YOUR_ENVIRONMENT>
# ...and your deployment mode's connection variables
EOF
aws s3 cp mirrord.env s3://<BUCKET>/mirrord/<YOUR_ENVIRONMENT>.env
```

and reference it from each application container instead of listing those variables:

```json
"environmentFiles": [
  { "type": "s3", "value": "arn:aws:s3:::<BUCKET>/mirrord/<YOUR_ENVIRONMENT>.env" }
]
```

`LD_PRELOAD` and `MIRRORD_REMOTE_SERVICE` stay in each container's `environment`, since they differ per service. Keep secrets, such as the MetalBear Cloud API key, out of the file, and inject them with `secrets` instead ([Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md#add-the-connection-variables)). Tasks read the file when they start, so redeploy the services after changing it. The task's **execution role** (not the task role) needs `s3:GetObject` on the file and `s3:GetBucketLocation` on the bucket. Environment files need Linux platform version **1.4.0 or later** on Fargate, or ECS container agent **1.39.0 or later** on EC2. If you don't use environment files, keep the variables in the task definition's `environment` list instead.

## Troubleshooting

These apply in every deployment mode. For connection and authentication problems, see your deployment mode's guide, such as [Connecting ECS to the Operator](ecs-operator-hosted.md#troubleshooting). The bootstrap's messages appear in the application container's logs.

| Symptom | Likely cause |
| --- | --- |
| The application starts without mirrord | The setup container failed or its `dependsOn` condition isn't wired up; check its logs and exit code. |
| The task registers but the developer is never paired | `MIRRORD_REMOTE_SERVICE`/`MIRRORD_REMOTE_ENVIRONMENT` on the task don't match `target.path`/`target.namespace` in `mirrord.json`. |
| `Read-only file system` or `Permission denied` under `/tmp` | The application container has no writable `/tmp` ([Prerequisites](#prerequisites)). |
| The application container stops at startup: version `GLIBC_2.30` not found, required by `libmirrord_remote_bootstrap.so` | The application image's glibc is older than 2.30 ([Prerequisites](#prerequisites)). |
