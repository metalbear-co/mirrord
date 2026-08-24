# Start the demo

This guide prepares the local frontend and backend. Once both are running, follow [`DEMO_FLOW.md`](./DEMO_FLOW.md).

## Prerequisites

- macOS with Git, Rust/Cargo, and Docker Desktop
- AWS credentials with access to the configured demo sessions manager and target
- The `aarch64-apple-darwin` Rust target on Apple Silicon:

```bash
rustup target add aarch64-apple-darwin
```

## 1. Check out the demo snapshot

From the mirrord repository root:

```bash
git remote add danielg git@github.com:itsamegraf/mirrord.git
git fetch danielg demo-snapshot-260804
git checkout -b danielg/demo-snapshot-260804 demo-snapshot-260804
```

If the `danielg` remote already exists, skip `git remote add` and run the fetch command.

## 2. Build mirrord

```bash
cargo xtask build-cli --no-ui
```

## 3. Start the demo UI

```bash
docker compose -f sample/capabilities-rust/docker-compose.yml up -d
```

Open <http://localhost:3000/demo>. Docker Compose starts the frontend; the backend is started separately below.

## 4. Start the backend under mirrord

From the mirrord repository root, in a second terminal:

```bash
set -a; . sample/capabilities-rust/demo/ecs/demo.env; set +a;
target/universal-apple-darwin/debug/mirrord exec \
  -f sample/capabilities-rust/demo/ecs/mirrord.json \
  cargo -- run --manifest-path sample/capabilities-rust/Cargo.toml \
  -p capabilities-rust-backend --target aarch64-apple-darwin
```

The `-f` path is relative to the mirrord repository root and points to the exact demo configuration: `sample/capabilities-rust/demo/ecs/mirrord.json`.

Keep this process running. The local backend should be available at <http://localhost:8080/healthz>.

Before starting the walkthrough, reset `sample/capabilities-rust/demo/ecs/mirrord.json` to the baseline configuration shown at the top of [`DEMO_FLOW.md`](./DEMO_FLOW.md). Keeping browser developer tools open with F12 is recommended.

The flow includes suggested code and `mirrord.json` edits. Add the suggested backend routes to `sample/capabilities-rust/backend/src/main.rs`, relative to the mirrord repository root, between the existing `/env` and `/outgoing` routes. After each requested code or configuration change, stop and rerun the local backend with the command above so the change is applied. This can be presented to customers as: “Now, to have my change applied, all I have to do is rerun the local backend through mirrord.”

The linked sessions-manager source is a separate sibling repository at `../operator/services/sessions-manager/src/main.rs` relative to the mirrord repository root; it is not the demo backend route file.

## 5. Follow the demo flow

With the frontend and backend running, follow [`DEMO_FLOW.md`](./DEMO_FLOW.md).

## Troubleshooting

- If the branch is unavailable, run `git fetch danielg demo-snapshot-260804`.
- If the CLI binary is missing, rerun `cargo xtask build-cli --no-ui`.
- If ports `3000` or `8080` are occupied, stop the conflicting process or run `docker compose -f sample/capabilities-rust/docker-compose.yml down` before restarting the UI.
- If mirrord cannot connect, check Docker, AWS credentials, `demo.env`, and the target configuration in `mirrord.json`.
