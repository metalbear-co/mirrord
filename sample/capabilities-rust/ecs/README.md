# capabilities-rust ECS setup

This folder contains the ECS-specific instructions and templates for the `capabilities-rust` demo.

The full demo has three workloads:

- `sessions-manager` - the control plane that `mirrord` connects to.
- `backend` - the Rust API that can run either normally or under `mirrord`.
- `frontend-next` - a Next.js UI for visibility.

## Files in this folder

- `sessions-manager.task-definition.json`
- `backend.task-definition.json`
- `frontend-next.task-definition.json`
- `sessions-manager.service-definition.json`
- `backend.service-definition.json`
- `frontend-next.service-definition.json`

Replace the placeholder image URIs, ARN values, VPC subnet IDs, and security group IDs before registering the task and service definitions.

## ECS deployment layout

The JSON files in this folder are intended to be used directly with the AWS CLI.

### 1. Build and push the images

These steps do not assume any images already exist.

Run the commands from the `operator` repository root and from `sample/capabilities-rust` as shown below:

```bash
# build the sessions-manager image in the operator checkout
# example ECR image URI:
# 123456789012.dkr.ecr.us-east-1.amazonaws.com/capabilities-rust-sessions-manager:latest
docker build -t mirrord-sessions-manager:local -f services/sessions-manager/Dockerfile .

# build the sample images in the mirrord checkout
docker build -t mirrord-deps-builder:local -f mirrord-dependancies-builder/Dockerfile ../..
docker build -t capabilities-rust-frontend-next:local -f frontend-next/Dockerfile frontend-next

# tag and push the deps image to ECR
# example ECR image URI:
# 123456789012.dkr.ecr.us-east-1.amazonaws.com/capabilities-rust-deps:latest
docker tag mirrord-deps-builder:local <deps-image-uri>
docker push <deps-image-uri>

# build the backend image from the pushed deps image
# the backend runtime only needs the serverless bootstrap shared object
docker build --build-arg MIRRORD_DEPS_IMAGE=<deps-image-uri> -t capabilities-rust-backend:local -f backend/Dockerfile .

# tag and push the app images to ECR
# replace these values with your registry URIs
docker tag mirrord-sessions-manager:local <sessions-manager-image-uri>
docker tag capabilities-rust-backend:local <backend-image-uri>
docker tag capabilities-rust-frontend-next:local <frontend-next-image-uri>
docker push <sessions-manager-image-uri>
docker push <backend-image-uri>
docker push <frontend-next-image-uri>
```

The `sessions-manager` container listens on `0.0.0.0:4971` and serves dataplane websocket upgrades at `/ws/{uuid}`.

### 2. Register the task definitions

Before registering the task definitions, update the placeholders in each template:

- `sessions-manager.task-definition.json`
  - `<sessions-manager-image-uri>`
  - `<ecsTaskExecutionRole-arn>`
  - `<taskRoleArn>`
- `backend.task-definition.json`
  - `<backend-image-uri>`
  - `<ecsTaskExecutionRole-arn>`
  - `<taskRoleArn>`
  - `<public-sessions-manager-url>`
- `frontend-next.task-definition.json`
  - `<frontend-next-image-uri>`
  - `<ecsTaskExecutionRole-arn>`
  - `<taskRoleArn>`

The JSON templates do not support inline comments, so the environment variables are documented here instead.

#### `sessions-manager.task-definition.json`

| Variable | Required? | Default / source | Notes |
| --- | --- | --- | --- |
| `RUST_LOG` | No | `info` from static code in `operator/services/sessions-manager/src/main.rs` | Controls the `sessions-manager` log level. |

#### `backend.task-definition.json`

| Variable | Required? | Default / source | Notes |
| --- | --- | --- | --- |
| `MIRRORD_AGENT_SIDECAR_SESSIONS_MANAGER_URL` | Yes | No in-repo default | Public URL of the ECS `sessions-manager` service. The sidecar needs this to reach ECS from inside the task. |
| `MIRRORD_SM_TENANT_ID` | Yes | No in-repo default | Tenant used to build the sessions-manager room id on the remote side. This must match the `MIRRORD_SM_TENANT_ID` value used by the local `mirrord exec` command. |
| `MIRRORD_SM_TARGET_ID` | Yes | No in-repo default | Target used to build the sessions-manager room id on the remote side. This must match the `MIRRORD_SM_TARGET_ID` value used by the local `mirrord exec` command. |
| `LD_PRELOAD` | Yes | No default | Loads `libmirrord_serverless_bootstrap.so` from the support image at `/opt/mirrord/lib/libmirrord_serverless_bootstrap.so`. |
| `DEMO_BIND_ADDR` | No | `0.0.0.0` from static code in `backend/src/main.rs` | Host part of the backend bind address. |
| `DEMO_BIND_PORT` | No | `8080` from static code in `backend/src/main.rs` | Port part of the backend bind address. |
| `DEMO_ENV` | No | No runtime default; set by this ECS template | Marker value for the demo. The application does not use it for configuration. |
| `RUST_LOG` | No | `error` from `tracing_subscriber::EnvFilter::from_default_env()` in `backend/src/main.rs` | Controls backend and bootstrap logging. |

The serverless bootstrap extracts the sidecar agent itself at runtime, so the task definition does not need to provide any additional mirrord binary paths.

#### `frontend-next.task-definition.json`

| Variable | Required? | Default / source | Notes |
| --- | --- | --- | --- |
| `PORT` | No | `3000` from `frontend-next/Dockerfile` | The runner image already sets the listening port. |

After updating the placeholders, register each task definition:

```bash
aws ecs register-task-definition --cli-input-json file://ecs/sessions-manager.task-definition.json
aws ecs register-task-definition --cli-input-json file://ecs/backend.task-definition.json
aws ecs register-task-definition --cli-input-json file://ecs/frontend-next.task-definition.json
```

The task-definition templates already point at the matching container names and ports.

### 3. Create the ECS services

Before creating the services, update the placeholders in each service-definition template:

- `sessions-manager.service-definition.json`
  - `<cluster-name>`
  - `<sessions-manager-target-group-arn>`
  - `<sessions-manager-security-group>`
  - `<public-subnet-1>`
  - `<public-subnet-2>`
- `backend.service-definition.json`
  - `<cluster-name>`
  - `<backend-security-group>`
  - `<private-or-public-subnet-1>`
  - `<private-or-public-subnet-2>`
- `frontend-next.service-definition.json`
  - `<cluster-name>`
  - `<frontend-target-group-arn>`
  - `<frontend-security-group>`
  - `<public-subnet-1>`
  - `<public-subnet-2>`

Create each service from the matching service-definition JSON file:

```bash
aws ecs create-service --cli-input-json file://ecs/sessions-manager.service-definition.json
aws ecs create-service --cli-input-json file://ecs/backend.service-definition.json
aws ecs create-service --cli-input-json file://ecs/frontend-next.service-definition.json
```

The service-definition files use the task-definition family names from the templates. If you prefer to pin a specific revision, replace `taskDefinition` with the full task definition ARN returned by `register-task-definition`.

#### `sessions-manager`

Use `sessions-manager.service-definition.json` to run `mirrord-sessions-manager` behind a public ALB or another internet-accessible endpoint.

Recommended settings:

- Container port: `4971`
- Protocol: HTTP/1.1 with websocket support
- Public reachability: enabled
- Security group: allow inbound from your laptop and from the backend task’s security group

#### `backend`

Use `backend.service-definition.json` to run `capabilities-rust-backend:local` on port `8080`.

The ECS task template sets the mirrord sidecar and serverless-bootstrap variables so the backend can connect to the public `sessions-manager` endpoint.

#### `frontend-next`

Use `frontend-next.service-definition.json` to run `capabilities-rust-frontend-next:local` on port `3000`.

This service does not need any `mirrord`-specific environment variables.

## Local backend against the ECS `sessions-manager`

This section is separate from the ECS deployment flow above.

Once `sessions-manager` is public, you can run the backend locally while it connects to ECS.

From `sample/capabilities-rust`:

```bash
MIRRORD_SESSIONS_MANAGER_URL=<public-sessions-manager-url> \
MIRRORD_SM_TENANT_ID=demo-tenant \
MIRRORD_SM_TARGET_ID=demo-target \
mirrord exec --bridge -f mirrord.json -- cargo run -p capabilities-rust-backend
```

What this does:

- `-f mirrord.json` loads the sample config.
- `--bridge` makes `mirrord` use `sessions-manager` directly.
- `MIRRORD_SM_TENANT_ID` and `MIRRORD_SM_TARGET_ID` define the room.
- `MIRRORD_SESSIONS_MANAGER_URL` must point at the public ECS endpoint.

The backend still listens on `8080` locally, so you can open the sample UI against `http://localhost:8080` once the process is running.
