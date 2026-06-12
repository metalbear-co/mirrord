# capabilities-rust

Sample app for mirrord capability demos:

- `backend`: APIs for health/env/meta/dns/outgoing/echo.
- `frontend-next`: Next.js (TypeScript) pane-based UI that calls each backend API directly.

## Layout

- `backend/` (`capabilities-rust-backend`)
- `frontend-next/` (Next.js + TypeScript)

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

- Frontend UI: `http://localhost:3000`
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

Set frontend backend URL in the top input (default `http://localhost:8080`).

## ECS notes

For ECS demo target/hijack app, deploy the backend container:

- image: `capabilities-rust-backend`
- container port: `8080`
- envs:
  - `DEMO_BIND_ADDR=0.0.0.0:8080`
  - `DEMO_ENV_PREFIX=DEMO_` (optional)
  - `DEMO_OUTGOING_TIMEOUT_MS=2000` (optional)

Frontend can run locally or in a separate task/container as a test UI.
