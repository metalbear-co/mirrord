# capabilities-rust

Sample app for mirrord capability demos.

- `backend`: APIs for health/env/meta/dns/outgoing/echo.
- `frontend-next`: Next.js (TypeScript) UI that calls the backend endpoints directly.

## Layout

- `backend/` (`capabilities-rust-backend`)
- `frontend-next/` (Next.js + TypeScript)
- `ecs/` (demo helper script, mirrord config, and deployment notes)

## Prerequisites

Before running the demo, log in to AWS and Docker first:

```bash
aws sts get-caller-identity
aws ecr get-login-password --region us-east-1 \
  | docker login --username AWS --password-stdin 013388577054.dkr.ecr.us-east-1.amazonaws.com
```

## Demo flow

This is the recommended flow for a live demo.

### 1. Open the AWS demo site

Open the public demo frontend:

- `http://demo.s8s.staging.metalbear.com/demo`

At this point, the backend is served from AWS and `/demo/api` should return the normal remote responses with no special header.

### 2. Show the normal AWS responses

In the frontend:

- keep the backend endpoint pointed at the AWS demo backend
- show `Env`, `Meta`, and `Outgoing HTTP`
- confirm the responses are coming from the AWS-backed backend

A good “baseline” is to hit `/demo/api/env` and `/demo/api/outgoing` without adding any `mirrord-session` header.

### 3. Run the backend locally under mirrord

Start a local backend with a config file such as `ecs/mirrord.json`.

If you want to use the same structure as the test config in `tests/rust-sqs-printer/mirrord.json`, the important part is the target selection and the session-key-based routing; the sample backend config uses the same `{{ key }}` templating style.

A typical local run looks like this:

```bash
MIRRORD_SESSIONS_MANAGER_URL=http://localhost:4971/sm \
MIRRORD_SM_TENANT_ID=demo-tenant \
MIRRORD_SM_TARGET_ID=demo-target \
RUST_LOG=debug \
mirrord exec -f ecs/mirrord.json -- cargo run -p capabilities-rust-backend
```

Use the local backend on `http://localhost:8080` as the target for this run.

### 4. Show local Env and Outgoing responses

With the local backend running under mirrord:

- point the frontend at `http://localhost:8080`
- show `Env`
- show `Outgoing HTTP`
- point the frontend back to the AWS backend and show the responses revert to the AWS-backed version

### 5. Turn on the session header

Add the header:

```text
mirrord-session=demo-serverless
```

For this demo, use it as a `baggage` header value, for example:

```text
baggage: mirrord-session=demo-serverless
```

Now the request should be stolen by the local backend when the config matches that key.

### 6. Show the request reaching the local backend

With the header enabled:

- send the request to the AWS frontend/backend path again
- show that the request is now handled by the local backend
- confirm the local process is answering instead of the AWS task

### 7. Explain the backend Dockerfile setup

The `backend/Dockerfile` has a `mirrord-remote` stage for running the container with the remote bootstrap and a `mirrord-local` stage for the local demo flow.

The remote stage does three important things:

- copies the remote bootstrap library into the image:

```dockerfile
COPY --from=mirrord-deps-builder:remote /opt/mirrord/lib/libmirrord_remote_bootstrap.so /opt/mirrord/lib/libmirrord_remote_bootstrap.so
```

- sets the session-manager tenant and target:

```dockerfile
ENV MIRRORD_SM_TENANT_ID=demo-tenant \
    MIRRORD_SM_TARGET_ID=demo-target \
    LD_PRELOAD=/opt/mirrord/lib/libmirrord_remote_bootstrap.so
```

Those three values are what make the backend container participate in the remote mirrord flow.

### 8. Optional: swap to mirror mode by editing the config

You can change the mirrord config and rerun the local backend to switch between demo modes without changing the app code.

That is a useful way to show how the same app can be pointed at different targets or session-routing setups.

### 9. Optional: demonstrate multiple people on the same remote backend

You can run multiple local mirrord sessions against the same remote `/demo/api` backend and route them independently by changing the header value.

For example:

- `mirrord-session=demo-serverless`
- `mirrord-session=local-docker-mirrord`

That lets different developers share the same remote service while still targeting their own local processes.

## Run locally

From `sample/capabilities-rust`:

```bash
cargo run -p capabilities-rust-backend
```

In another terminal (preferred frontend):

```bash
cd frontend-next
npm install
npm run dev
```

Then open:

- Frontend UI: `http://localhost:3000/demo`
- Backend direct: `http://localhost:8080/healthz`

## Backend endpoints

- `GET /healthz`
- `GET /meta`
- `GET /env`
- `GET /dns?host=example.com`
- `GET /outgoing?url=https://example.com`
- `POST /echo` (raw body echo + headers)

## Run in Docker (local)

Backend:

```bash
docker build -t capabilities-rust-backend:local -f backend/Dockerfile .
docker run --rm -p 8080:8080 -e DEMO_ENV_PREFIX=DEMO_ capabilities-rust-backend:local
```

Frontend (Next.js):

```bash
docker build -t capabilities-rust-frontend-next:local -f frontend-next/Dockerfile frontend-next
docker run --rm -p 3000:3000 capabilities-rust-frontend-next:local
```

The frontend exposes saved endpoint pills for the three backend variants, so you can switch between them without editing the URL manually.

## How to test

1. Start `sessions-manager` on the host and keep it listening on `http://localhost:4971/sm`.
2. From `sample/capabilities-rust`, run:

```bash
docker compose up --build backend backend-remote backend-mirrord frontend
```

3. Open `http://localhost:3000/demo` and use the saved endpoint pills to compare:
   - `vanilla (:8082)`
   - `mirrord -> remote (:8080)`
   - `remote (running agent) (:8081)`
4. In the `Env` pane, verify `values.DEMO_ENV`:
   - `vanilla` returns `no_mirrord`
   - `remote` returns `mirrord-remote`
   - `backend-mirrord` matches the remote-backed result
5. Optional: use `Meta` as a control (`pid`/`hostname` differ between `backend-mirrord` and `backend-remote`), or check `DNS`/`Outgoing HTTP` for a stronger remote-execution signal.
