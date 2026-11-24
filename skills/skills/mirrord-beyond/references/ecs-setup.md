# ECS setup reference (MetalBear Cloud)

Commands and rules for Track 1 of the `mirrord-beyond` skill: making an existing Amazon ECS service targetable through the MetalBear Cloud sessions-manager. Source: [Amazon ECS](https://metalbear.com/mirrord/docs/using-mirrord/beyond-kubernetes/ecs) and [Connecting ECS to MetalBear Cloud](https://metalbear.com/mirrord/docs/using-mirrord/beyond-kubernetes/ecs-metalbear-cloud).

Placeholders: `<REGION>`, `<CLUSTER>`, `<SERVICE>` (the ECS service), `<APP_CONTAINER>` (the application container's name in the task definition), `<SERVICE_NAME>` (the mirrord Beyond service name developers will target, often the same as `<SERVICE>`), `<ENVIRONMENT>` (optional; unset means `default`), `<EXECUTION_ROLE_NAME>`, `<SECRET_ARN>`. Use the AWS profile and region the user already has configured; pass `--region <REGION>` explicitly when the service's region differs.

## Preflight

Work in a scratch directory (`cd "$(mktemp -d)"`), and delete it when the setup is done: the files below hold the task definition, which is configuration the user may not want left around.

Find the service and its current task definition:

```bash
aws sts get-caller-identity
aws configure get region
aws ecs list-clusters
aws ecs list-services --cluster <CLUSTER>
aws ecs describe-services --cluster <CLUSTER> --services <SERVICE> \
  --query 'services[0].{taskDefinition:taskDefinition,launchType:launchType,platformVersion:platformVersion,capacityProviders:capacityProviderStrategy[].capacityProvider,desiredCount:desiredCount}'
aws ecs describe-task-definition --task-definition <TASK_DEFINITION_ARN> \
  --include TAGS > described.json
jq '.taskDefinition + ([.tags[]? | select(.key | startswith("aws:") | not)] | if . == [] then {} else {tags: .} end)' described.json > taskdef.json
jq '{executionRoleArn, requiresCompatibilities, memory, containers: [.containerDefinitions[] | {name, image, essential, readonlyRootFilesystem, ld_preload: ([.environment[]? | select(.name == "LD_PRELOAD") | .value] | first)}]}' taskdef.json
```

The second command keeps the task definition's tags, so the new revision has them too, except the reserved `aws:` ones, which `register-task-definition` refuses.

Ask the user whether the service is managed by Terraform, CDK, CloudFormation or another IaC tool. If it is, a revision registered from the CLI is reverted by the next apply, and a grant added from the CLI may be removed if the IaC manages the execution role's inline policies. Make the changes in their IaC instead (see [Infrastructure as code](#variants)): edit it if it's in the workspace, or give them the changes. Still create the secret's value from the CLI, so the key stays out of the IaC code and state. Use CLI writes against IaC-managed resources only for a throwaway trial the user explicitly agrees to.

Run every check below (they're all read-only), then report all unmet requirements together, and don't continue past this step until they're resolved:

| Requirement | How to check |
| --- | --- |
| An execution role | `executionRoleArn` is set. Its name is the last path segment of the ARN. Without one, ECS can't inject the secret; the user has to add one. |
| Linux, glibc 2.30 or later, not musl (Alpine) | With Docker available and the image pullable: `docker run --rm --entrypoint ldd <APP_IMAGE> --version`. The first line shows the glibc version, or `musl`. Otherwise ask the user what the image is based on. Amazon Linux 2, RHEL 8 and Debian 10 are too old. |
| A writable `/tmp` in the application container | If the container sets `readonlyRootFilesystem: true`, it needs a task volume mounted at `/tmp` ([Variants](#variants)). |
| `SUCCESS` container dependencies | Fargate: platform version 1.3.0 or later (`LATEST` is fine). EC2: ECS container agent 1.26.0 or later. |
| Outbound HTTPS | Tasks reach `app.metalbear.com` and `sm-aws-use1.app.metalbear.com` on TCP 443: security group egress allows 443; private subnets route through a NAT gateway, public ones assign a public IP; any egress proxy or firewall allows both hosts (or `app.metalbear.com` plus `*.app.metalbear.com`), doesn't buffer responses, and has an idle timeout of at least 300 seconds. |
| No `LD_PRELOAD` on the application container other than mirrord's own `/opt/mirrord/lib/libmirrord_remote_bootstrap.so` (which is there on a rerun or upgrade, and is fine) | Check its `environment` (the summary above), any `environmentFiles`, and the image's `ENV` (`docker image inspect <APP_IMAGE> --format '{{json .Config.Env}}'` when Docker is available; otherwise ask the user). The task's `environment` overrides both, so a value there would be lost silently. The mirrord docs don't cover combining the bootstrap with another preloaded library (jemalloc, profilers), and the merge refuses to overwrite one. If one is set, stop: tell the user, and suggest they ask MetalBear support before going further. Don't combine the values yourself. |
| No variables of the Operator-hosted mode | The application container doesn't set `MIRRORD_OPERATOR_API_URL`, `MIRRORD_OPERATOR_KUBE_CONTEXT` or `MIRRORD_SESSIONS_MANAGER_URL`. |

If `desiredCount` is more than 1, tell the user that a developer's session is served by one task only, so only requests that land on that task reach them. Scaling to one task for testing is their call.

## Store the API key

One secret serves every ECS service in a region. Look for it first:

```bash
aws secretsmanager describe-secret --region <REGION> \
  --secret-id mirrord/sessions-manager-api-key --query ARN --output text
```

**It exists:** use the printed ARN as `<SECRET_ARN>` and skip to the grant. No API key is needed. Don't change the secret's value unless the user asks (for example after a [rotation](#rotating-the-key)).

**It doesn't exist:** the key has to come from the user. An organization admin generates it at [app.metalbear.com](https://app.metalbear.com) (the onboarding wizard's "Get your agent key" step, or Settings). It's shown once and begins with `metalbear_key_`. The user exports it in their own terminal and starts the agent from that terminal. Reading it from a prompt keeps it out of the shell history (this is bash; in zsh, the default on macOS, use `read -rs "MIRRORD_API_KEY?mirrord API key: " && export MIRRORD_API_KEY`):

```bash
read -rsp 'mirrord API key: ' MIRRORD_API_KEY && export MIRRORD_API_KEY
```

Check that it reached your shell without reading it:

```bash
test -n "$MIRRORD_API_KEY" && echo set
```

Then create the secret. `printf` is a shell builtin and the AWS CLI reads the value from standard input, so the key goes to AWS without passing through the chat or appearing in any process's arguments:

```bash
printf %s "$MIRRORD_API_KEY" | aws secretsmanager create-secret --region <REGION> \
  --name mirrord/sessions-manager-api-key \
  --secret-string file:///dev/stdin \
  --query ARN --output text
```

If the check prints nothing, don't ask for the key. Offer the user two ways on: restart the agent from a terminal that exports `MIRRORD_API_KEY`, or run the `create-secret` command above in their own terminal and give you only the ARN it prints. Everything after this step needs only the ARN.

Let the task's **execution role** (not the task role) read the secret:

```bash
aws iam put-role-policy --role-name <EXECUTION_ROLE_NAME> \
  --policy-name mirrord-sessions-manager-api-key \
  --policy-document '{
    "Version": "2012-10-17",
    "Statement": [{
      "Effect": "Allow",
      "Action": "secretsmanager:GetSecretValue",
      "Resource": "<SECRET_ARN>"
    }]
  }'
```

Each ECS service may have its own execution role, so run the grant for every service you onboard, even when the secret already existed. If the secret is encrypted with a customer-managed KMS key rather than `aws/secretsmanager`, also allow `kms:Decrypt` on that key.

Check both:

```bash
aws secretsmanager describe-secret --region <REGION> --secret-id mirrord/sessions-manager-api-key --query ARN --output text
aws iam get-role-policy --role-name <EXECUTION_ROLE_NAME> --policy-name mirrord-sessions-manager-api-key
```

## Merge the task definition

The merge adds, to the task definition from [Preflight](#preflight):

- a task volume `mirrord`;
- a non-essential setup container, `install-mirrord-remote-bootstrap`, that copies the bootstrap library into the volume;
- on the application container: a `SUCCESS` dependency on the setup container, the volume mounted read-only at `/opt/mirrord/lib`, the `LD_PRELOAD`, `MIRRORD_REMOTE_SERVICE` and (when set) `MIRRORD_REMOTE_ENVIRONMENT` variables, and the API key secret as `MIRRORD_SESSIONS_MANAGER_API_KEY`.

It keeps every existing field, container, variable, secret, mount and dependency; changes only the named application container (other containers, including any that already load mirrord, stay as they are); replaces mirrord's own entries on a rerun instead of duplicating them, keeping settings added to the setup container such as `logConfiguration` (so it's also how to upgrade the bootstrap tag); and removes the read-only fields `register-task-definition` rejects. It stops with an error if the container doesn't exist or is the setup container itself, if the container already sets a different `LD_PRELOAD`, or if the task already has a configured volume named `mirrord`.

If the preflight summary shows mirrord's `LD_PRELOAD` on a container other than the one you're onboarding (for example, it was added to the wrong container earlier), ask the user whether that container should keep it. If not, remove mirrord's entries from it by hand (`LD_PRELOAD`, `MIRRORD_REMOTE_*`, `MIRRORD_SESSIONS_MANAGER_API_KEY`, the `mirrord` mount, the dependency on the setup container) and show them the diff.

Save this filter as `merge.jq`:

```jq
# mirrord Beyond: merge the remote bootstrap into an ECS task definition.
# Input: the .taskDefinition object of `aws ecs describe-task-definition`.
# Args: container, service, environment ("" = default), secret_arn, image, ec2_memory (true/false).
def upsert($key; $items):
  ((. // []) | map(. as $old | select(any($items[]; .[$key] == $old[$key]) | not))) + $items;
"/opt/mirrord/lib/libmirrord_remote_bootstrap.so" as $lib
| "install-mirrord-remote-bootstrap" as $setup
| if $container == $setup then error("\($setup) is mirrord's setup container, not the application container")
  elif any(.containerDefinitions[]; .name == $container) then .
  else error("no container named \"\($container)\" in the task definition; its containers are: \([.containerDefinitions[].name] | join(", "))")
  end
| if any(.volumes[]?; .name == "mirrord" and (keys - ["name"]) != []) then
    error("the task definition already has a configured volume named \"mirrord\"; stop and ask the user")
  else . end
| ((.containerDefinitions[] | select(.name == $setup)) // {}) as $old_setup
| ([.containerDefinitions[] | select(.name == $container) | .environment[]? | select(.name == "LD_PRELOAD" and .value != $lib)] | first) as $preload
| if $preload == null then .
  else error("container \"\($container)\" already sets LD_PRELOAD=\($preload.value); stop and ask the user before combining it with \($lib)")
  end
| del(.taskDefinitionArn, .revision, .status, .requiresAttributes, .compatibilities, .registeredAt, .registeredBy)
| .volumes |= upsert("name"; [{name: "mirrord"}])
| .containerDefinitions |= (
    map(
      if .name == $container then
        .dependsOn |= upsert("containerName"; [{containerName: $setup, condition: "SUCCESS"}])
        | .mountPoints |= upsert("sourceVolume"; [{sourceVolume: "mirrord", containerPath: "/opt/mirrord/lib", readOnly: true}])
        | .environment |= ((. // []) | map(select(.name != "MIRRORD_REMOTE_ENVIRONMENT"))
            | upsert("name"; [
                {name: "LD_PRELOAD", value: $lib},
                {name: "MIRRORD_REMOTE_SERVICE", value: $service}
              ] + (if $environment == "" then [] else [{name: "MIRRORD_REMOTE_ENVIRONMENT", value: $environment}] end)))
        | .secrets |= upsert("name"; [{name: "MIRRORD_SESSIONS_MANAGER_API_KEY", valueFrom: $secret_arn}])
      else . end
    )
    # On a rerun, keep any settings added to the setup container (logging, registry credentials).
    | upsert("name"; [
        $old_setup + {
          name: $setup,
          image: $image,
          essential: false,
          entryPoint: ["/bin/sh", "-c"],
          command: ["cp /opt/mirrord/lib/libmirrord_remote_bootstrap.so /mirrord/libmirrord_remote_bootstrap.so"],
          mountPoints: [{sourceVolume: "mirrord", containerPath: "/mirrord"}]
        } + (if $ec2_memory then {memoryReservation: 32} else {} end)
      ])
  )
```

Run it:

```bash
jq --arg container '<APP_CONTAINER>' \
   --arg service '<SERVICE_NAME>' \
   --arg environment '' \
   --arg secret_arn '<SECRET_ARN>' \
   --arg image ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.229.0 \
   --argjson ec2_memory false \
   -f merge.jq taskdef.json > taskdef-mirrord.json
```

- `environment`: leave it `""` unless the user wants a named environment; developers then leave `target.namespace` unset too. If one of the container's `environmentFiles` sets `MIRRORD_REMOTE_ENVIRONMENT`, the merge doesn't see it and the task registers under that value: pass the same value here, or remove it from the file, so what you tell developers matches.
- `ec2_memory`: `true` only for the EC2 launch type when the task definition has no task-level `memory`, since ECS then needs a memory setting on every container. It gives the setup container `memoryReservation: 32` (MiB).
- `image`: pin a release tag, `3.229.0` or later. Never `latest`. Bootstrap tags follow Operator releases, not CLI versions.

Show the user what changed before registering:

```bash
diff <(jq -S . taskdef.json) <(jq -S . taskdef-mirrord.json)
```

The task-definition fragment the merge produces, for reference or for IaC:

```json other
{
  "volumes": [{ "name": "mirrord" }],
  "containerDefinitions": [
    {
      "name": "install-mirrord-remote-bootstrap",
      "image": "ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.229.0",
      "essential": false,
      "entryPoint": ["/bin/sh", "-c"],
      "command": ["cp /opt/mirrord/lib/libmirrord_remote_bootstrap.so /mirrord/libmirrord_remote_bootstrap.so"],
      "mountPoints": [{ "sourceVolume": "mirrord", "containerPath": "/mirrord" }]
    },
    {
      "name": "<APP_CONTAINER>",
      "dependsOn": [{ "containerName": "install-mirrord-remote-bootstrap", "condition": "SUCCESS" }],
      "mountPoints": [{ "sourceVolume": "mirrord", "containerPath": "/opt/mirrord/lib", "readOnly": true }],
      "environment": [
        { "name": "LD_PRELOAD", "value": "/opt/mirrord/lib/libmirrord_remote_bootstrap.so" },
        { "name": "MIRRORD_REMOTE_SERVICE", "value": "<SERVICE_NAME>" }
      ],
      "secrets": [{ "name": "MIRRORD_SESSIONS_MANAGER_API_KEY", "valueFrom": "<SECRET_ARN>" }]
    }
  ]
}
```

## Register and deploy

Registering a new revision doesn't affect running tasks:

```bash
aws ecs register-task-definition --cli-input-json file://taskdef-mirrord.json \
  --query taskDefinition.taskDefinitionArn --output text
```

Before deploying, tell the user the cluster, the service and the new revision ARN, and wait for their explicit go-ahead. Deploying replaces the service's running tasks. Then:

```bash
aws ecs update-service --cluster <CLUSTER> --service <SERVICE> --task-definition <NEW_TASK_DEFINITION_ARN>
aws ecs wait services-stable --cluster <CLUSTER> --services <SERVICE>
```

Because the setup container is non-essential with a `SUCCESS` dependency, a failed copy stops the application container from starting, so the service never runs silently without mirrord. `services-stable` times out after about 10 minutes; if it does, go to [Verify](#verify) to see why.

## Verify

```bash
aws ecs list-tasks --cluster <CLUSTER> --service-name <SERVICE> --query 'taskArns' --output text
aws ecs describe-tasks --cluster <CLUSTER> --tasks <TASK_ARN> \
  --query 'tasks[0].{status:lastStatus,stoppedReason:stoppedReason,taskDefinition:taskDefinitionArn,containers:containers[].{name:name,status:lastStatus,exitCode:exitCode,reason:reason}}'
```

1. The task runs the new revision.
2. `install-mirrord-remote-bootstrap` stopped with `exitCode` 0.
3. The application container is `RUNNING`.
4. The application container's logs show no bootstrap, sessions-manager or API key errors. With the `awslogs` driver, the log group is in the container's `logConfiguration.options."awslogs-group"`: `aws logs tail <LOG_GROUP> --since 15m`. Compare anything suspicious with [troubleshooting](troubleshooting.md#app-container-logs). To see the registration itself, add `MIRRORD_AGENT_RUST_LOG=info` to the container's `environment`; the logs then show `Registering with the MetalBear Cloud sessions-manager`.

The app.metalbear.com onboarding wizard marks the step "Task connected" once a task has exchanged the API key for a token. That proves the key works. It doesn't prove the names match what developers will use, so still check the logs, and finish with a developer session (Track 2).

## Rotating the key

1. An organization admin rotates the key in Settings at app.metalbear.com and picks a grace window (revoke now, in 1 hour, in 1 day or in 7 days). The old key keeps working until the window ends.
2. The user exports the new key as `MIRRORD_API_KEY` (as in [Store the API key](#store-the-api-key)), and you update the secret:

   ```bash
   printf %s "$MIRRORD_API_KEY" | aws secretsmanager put-secret-value --region <REGION> \
     --secret-id mirrord/sessions-manager-api-key \
     --secret-string file:///dev/stdin
   ```

3. Redeploy every service that uses the secret before the window ends, after the user's go-ahead. Tasks read the secret only when they start:

   ```bash
   aws ecs update-service --cluster <CLUSTER> --service <SERVICE> --force-new-deployment
   ```

Once the old key is revoked, new registrations with it are refused; sessions already open continue until they end.

## Variants

**EC2 without task-level memory.** Run the merge with `--argjson ec2_memory true`.

**Tasks can't pull from `ghcr.io`.** Copy the bootstrap image to a registry the tasks can pull from, such as Amazon ECR, keeping the tag, and pass that image to the merge as `image`.

**Read-only root filesystem.** Add a task volume (for example `{"name": "tmp"}`) and mount it on the application container at `/tmp`, alongside the merge. Show the user the change before registering.

**Sharing settings across services with an environment file.** Non-secret values that are the same for every service, such as `MIRRORD_REMOTE_ENVIRONMENT`, can live in an S3 environment file referenced from each application container's `environmentFiles`. `LD_PRELOAD` and `MIRRORD_REMOTE_SERVICE` stay in each container's `environment`, and the API key always stays in `secrets`. The execution role needs `s3:GetObject` on the file and `s3:GetBucketLocation` on the bucket; environment files need Fargate platform version 1.4.0 or later, or ECS container agent 1.39.0 or later on EC2. Tasks read the file when they start.

**Infrastructure as code.** Each step maps to standard Terraform resources:

| Step | Terraform resources |
| --- | --- |
| Store the API key | `aws_secretsmanager_secret` (the secret only, not its value), `aws_iam_role_policy` |
| Allow outbound HTTPS | `aws_vpc_security_group_egress_rule` |
| Merge the task definition | `aws_ecs_task_definition` (the fragment in [Merge the task definition](#merge-the-task-definition)) |

Keep the API key out of the Terraform code and state: don't put it in a secret version resource, whose value Terraform stores in state. Set the value from the CLI with the `put-secret-value` command in [Rotating the key](#rotating-the-key), after Terraform creates the secret, or create the whole secret from the CLI and reference it by name.
