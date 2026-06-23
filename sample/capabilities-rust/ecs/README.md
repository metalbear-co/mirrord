# capabilities-rust ECS setup

This folder contains the ECS-specific instructions and templates for the `capabilities-rust` demo.

The full demo has three workloads:

- `sessions-manager` - the control plane that `mirrord` connects to.
- `backend` - the Rust API that can run either normally or under `mirrord`.
- `frontend-next` - a Next.js UI for visibility.

## Prepare `mirrord` with `sessions-manager` connectivity

Make sure you have an updated `mirrord`, go to the `mirrord` repository root and build the latest CLI:

```bash
cargo xtask build-cli --no-wizard --no-monitor
```

Assuming you are running on mac, the new binary will be in `target/universal-apple-darwin/debug/mirrord`
otherwise, `target/<target-triplet>/debug/mirrrod`

## ECS deployment layout

The JSON files in this folder are intended to be used directly with the AWS CLI.

### 1. Build and push the images

These steps do not assume any images already exist.

Run the following commands from the `operator` repository root

```bash
# build the sessions-manager image in the operator checkout root
docker build -t mirrord-sessions-manager:local -f services/sessions-manager/Dockerfile .
# replace <sessions-manager-image-uri> with your registry URIs, example ECR image URI:
# 123456789012.dkr.ecr.us-east-1.amazonaws.com/mirrord-sessions-manager:latest
docker tag mirrord-sessions-manager:local <sessions-manager-image-uri>
docker push <sessions-manager-image-uri>
```

Run the following commands in the mirrord repository, under `sample/capabilities-rust`

```bash
# build the sample images in the mirrord checkout from `sample/capabilities-rust`
docker build -t mirrord-deps-builder:local -f mirrord-dependancies-builder/Dockerfile ../..
docker build -t capabilities-rust-frontend-next:local -f frontend-next/Dockerfile frontend-next
docker build -t capabilities-rust-backend:local -f backend/Dockerfile .

# tag and push the app images to ECR
# replace the image-uri placeholders with your registry URIs, example ECR image URI:
# 123456789012.dkr.ecr.us-east-1.amazonaws.com/mirrord-sessions-manager:latest
docker tag capabilities-rust-frontend-next:local <frontend-next-image-uri>
docker tag capabilities-rust-backend:local <backend-image-uri>
docker push <frontend-next-image-uri>
docker push <backend-image-uri>
```

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
| `MIRRORD_SESSIONS_MANAGER_URL` | Yes | `<public-sessions-manager-url>` in `backend.task-definition.json` | Public URL of the ECS `sessions-manager` service. The sidecar needs this to reach ECS from inside the task. must begin with `http://` |
| `MIRRORD_SM_TENANT_ID` | Yes | `demo-tenant` in `backend.task-definition.json` | Tenant used to build the sessions-manager room id on the remote side. This must match the `MIRRORD_SM_TENANT_ID` value used by the local `mirrord exec` command. |
| `MIRRORD_SM_TARGET_ID` | Yes | `demo-target` in `backend.task-definition.json` | Target used to build the sessions-manager room id on the remote side. This must match the `MIRRORD_SM_TARGET_ID` value used by the local `mirrord exec` command. |
| `LD_PRELOAD` | Yes | `/opt/mirrord/lib/libmirrord_serverless_bootstrap.so` in `backend.task-definition.json` | Loads `libmirrord_serverless_bootstrap.so` from the support image at `/opt/mirrord/lib/libmirrord_serverless_bootstrap.so`. |
| `DEMO_BIND_ADDR` | No | `0.0.0.0` from static code in `backend/src/main.rs` | Host part of the backend bind address. |
| `DEMO_BIND_PORT` | No | `8080` from static code in `backend/src/main.rs` | Port part of the backend bind address. |
| `DEMO_ENV` | No | No runtime default; set by this ECS template | Marker value for the demo. The application does not use it for configuration. |
| `RUST_LOG` | No | `error` from `tracing_subscriber::EnvFilter::from_default_env()` in `backend/src/main.rs` | Controls backend and bootstrap logging. |

The serverless bootstrap extracts the sidecar agent itself at runtime, so the task definition does not need to provide any additional mirrord binary paths.

#### `frontend-next.task-definition.json`

| Variable | Required? | Default / source | Notes |
| --- | --- | --- | --- |
| `PORT` | No | `3000` from `frontend-next/Dockerfile` | The runner image already sets the listening port. |

After updating the placeholders, register each task definition.
Run the AWS CLI commands from `sample/capabilities-rust` so the `file://ecs/...` paths resolve correctly:

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

The service-definition files use the task-definition family names from the templates. If you prefer to pin a specific revision, replace `taskDefinition` with the full task definition ARN returned by `register-task-definition`.

#### `sessions-manager`

Use `sessions-manager.service-definition.json` to run `mirrord-sessions-manager` behind a public ALB or another internet-accessible endpoint.

Create the service:

```bash
aws ecs create-service --cli-input-json file://ecs/sessions-manager.service-definition.json
```

Recommended settings:

- Container port: `4971`
- Protocol: HTTP/1.1 with websocket support
- Public reachability: enabled
- Security group: allow inbound from your laptop and from the backend task’s security group

#### `backend`

Use `backend.service-definition.json` to run `capabilities-rust-backend:local` on port `8080`.

The ECS task template sets the mirrord sidecar and serverless-bootstrap variables so the backend can connect to the public `sessions-manager` endpoint.

Create the service:

```bash
aws ecs create-service --cli-input-json file://ecs/backend.service-definition.json
```

#### `frontend-next`

Use `frontend-next.service-definition.json` to run `capabilities-rust-frontend-next:local` on port `3000`.

Create the service:

```bash
aws ecs create-service --cli-input-json file://ecs/frontend-next.service-definition.json
```

## Run `mirrord` backend against the ECS `sessions-manager`

This section is separate from the ECS deployment flow above.

Make sure `sessions-manager` is already running before you start the backend, otherwise the local `mirrord exec` session cannot connect.

Once `sessions-manager` is public, you can run the backend locally while it connects to ECS.

Then run the backend from `sample/capabilities-rust` using the built binary at `target/universal-apple-darwin/debug/mirrord`:

```bash
MIRRORD_SESSIONS_MANAGER_URL=<public-sessions-manager-url> \
MIRRORD_SM_TENANT_ID=demo-tenant \
MIRRORD_SM_TARGET_ID=demo-target \
../../target/universal-apple-darwin/debug/mirrord exec --bridge -f mirrord.json -- cargo run -p capabilities-rust-backend
```

What this does:

- `-f mirrord.json` loads the sample config.
- `--bridge` makes `mirrord` use `sessions-manager` directly.
- `MIRRORD_SM_TENANT_ID` and `MIRRORD_SM_TARGET_ID` define the room.
- `MIRRORD_SESSIONS_MANAGER_URL` must point at the public ECS endpoint.

The backend will listen on `8080` locally and run the requests against the remote 

### 4. DEMO TIME

Browse to the frontend service at `http://<frontend-public-url>:3000` using the frontend's default port.
If you changed the frontend port with the `PORT` env var, use that port instead.

In the frontend, make sure the selected backend endpoint points to your local backend:

- label: `mirrord -> remote (:8080)`
- url: `http://localhost:8080`

Then open the `Outgoing HTTP` pane and set the outgoing field to:

```text
http://169.254.170.2/v2/metadata
```

Run the request. This should exercise the backend through the frontend and return the Amazon ECS task metadata endpoint v2 response from inside the ECS task.

## Fallback: Local Demo

If you don’t want to run the ECS pieces, you can use the images you built locally in step 1 and keep everything on your machine. You do not need to push them to ECR for this fallback flow.

### 1. Run `sessions-manager` locally

From `sample/capabilities-rust`:

```bash
docker run --rm -d --name sessions-manager -p 4971:4971 mirrord-sessions-manager:local
```

The service listens on `http://localhost:4971`.

### 2. Run `frontend` locally

From `sample/capabilities-rust`:

```bash
docker run --rm -d --name capabilities-rust-frontend -p 3000:3000 capabilities-rust-frontend-next:local
```

Open `http://localhost:3000` once the container is ready.

### 3. Run `backend` locally as remote

From `sample/capabilities-rust`:

```bash
docker run --rm -d --name capabilities-rust-backend-remote \
  -e MIRRORD_SESSIONS_MANAGER_URL=http://host.docker.internal:4971 \
  -e MIRRORD_SM_TENANT_ID=demo-tenant \
  -e MIRRORD_SM_TARGET_ID=demo-target \
  -e LD_PRELOAD=/opt/mirrord/lib/libmirrord_serverless_bootstrap.so \
  -e RUST_LOG=debug \
  capabilities-rust-backend:local
```

Note: LD_PRELOAD path should not be adjusted, it was copied to their from `mirrord-deps` image in `backend/Dockerfile`

Notice: we don't publish the port of the container, we will mirrord into it.
if you want to access it directly, add `-p 8081:8080` and target it in the frontend as `localhost:8081`.

### 4. Run `mirrord` backend 

```bash
MIRRORD_SESSIONS_MANAGER_URL=http://localhost:4971 \
MIRRORD_SM_TENANT_ID=demo-tenant \
MIRRORD_SM_TARGET_ID=demo-target \
RUST_LOG=debug \
../../target/universal-apple-darwin/debug/mirrord exec --bridge -f mirrord.json -- cargo run -p capabilities-rust-backend
```

`backend` listens on 8080 by default, target it in the frontend will redirect (using mirrord) the requests to the `capabilities-rust-backend-remote` container.

You can verify mirrord execution by:
* running a dns query to "host.docker.internal" - will give different values from running on the host (where we run the step-4) 
* making an env request and seeing the container's HOSTNAME in the env vars
