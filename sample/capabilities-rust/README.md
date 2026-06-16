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

The frontend exposes saved endpoint pills for the three backend variants, so you can switch between them without editing the URL manually.

## How to test

1. Start `sessions-manager` on the host and keep it listening on port `4971`.
2. From `sample/capabilities-rust`, run:

```bash
docker compose up --build backend backend-remote backend-mirrord frontend
```

3. Open `http://localhost:3000` and use the saved endpoint pills to compare:
   - `vanilla (:8082)`
   - `mirrord -> remote (:8080)`
   - `remote (running agent) (:8081)`
4. In the `Env` pane, verify `values.DEMO_ENV`:
   - `vanilla` returns `no_mirrord`
   - `remote` returns `mirrord-remote`
   - `backend-mirrord` matches the remote-backed result
5. Optional: use `Meta` as a control (`pid`/`hostname` differ between `backend-mirrord` and `backend-remote`), or check `DNS`/`Outgoing HTTP` for a stronger remote-execution signal.
