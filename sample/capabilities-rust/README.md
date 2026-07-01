# capabilities-rust

Sample app for mirrord capability demos.

- `backend`: Rust API with health/env/meta/dns/outgoing/echo endpoints.
- `frontend-next`: Next.js UI that can call each backend endpoint directly.

## Local run

From `sample/capabilities-rust`:

```bash
cargo run -p capabilities-rust-backend
```

In another terminal:

```bash
cd frontend-next
npm install
npm run dev
```

Then open:

- Frontend UI: `http://localhost:3000`
- Backend direct: `http://localhost:8080/healthz`

## Local Docker run

Backend:

```bash
docker build -t capabilities-rust-backend:local -f backend/Dockerfile .
docker run --rm -p 8080:8080 capabilities-rust-backend:local
```

Frontend:

```bash
docker build -t capabilities-rust-frontend-next:local -f frontend-next/Dockerfile frontend-next
docker run --rm -p 3000:3000 capabilities-rust-frontend-next:local
```

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
