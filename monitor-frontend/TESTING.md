# Session Monitor Testing

Test checklist for the combined session monitor feature. Run these before updating the combined PR.

## Quick run (automated)

Build the binary first:
```bash
LAYER_PATH="$(pwd)/target/debug/libmirrord_layer.dylib"
MIRRORD_LAYER_FILE="$LAYER_PATH" MIRRORD_LAYER_FILE_MACOS_ARM64="$LAYER_PATH" cargo build -p mirrord
```

### Rust checks
```bash
cargo check -p mirrord-intproxy --keep-going
cargo test -p mirrord-intproxy          # 37+ tests
cargo clippy -p mirrord-intproxy        # 0 warnings
cargo fmt -- --check                    # no diff
MIRRORD_LAYER_FILE_MACOS_ARM64=/dev/null cargo check -p mirrord --keep-going
```

### Frontend checks
```bash
cd monitor-frontend && npm ci && npm run build && npx tsc --noEmit
```

### mirrord ui smoke test
```bash
./target/debug/mirrord ui --no-open --port 59284 &
UI_PID=$!
sleep 2

# Frontend serves
curl -s -o /dev/null -w "%{http_code}" http://localhost:59284/         # expect 200
# API returns empty sessions
curl -s http://localhost:59284/api/sessions                             # expect []
# Static assets served
curl -s -o /dev/null -w "%{http_code}" http://localhost:59284/assets/   # expect 200 on CSS/JS

kill $UI_PID
```

### mirrord ui --dev mode
```bash
./target/debug/mirrord ui --no-open --dev --port 59285 &
DEV_PID=$!
sleep 2

# API still works
curl -s http://localhost:59285/api/sessions                             # expect []
# Static files NOT served (proxied to Vite)
curl -s -o /dev/null -w "%{http_code}" http://localhost:59285/         # expect non-200

kill $DEV_PID
```

## Integration: mirrord exec + socket API

Requires a working `mirrord exec` (layer dylib must match target arch).
On macOS with arm64 layer, use an arm64 target process (not x86_64 `sleep`).

```bash
# Start a mirrord session
./target/debug/mirrord exec --target pod/<target-pod> -- sleep 30 &
MIRRORD_PID=$!
sleep 5

# Find the session socket
SOCK=$(find ~/.mirrord/sessions/ -name "*.sock" | head -1)

# Test API endpoints
curl -s --unix-socket "$SOCK" http://localhost/health     # {"status":"ok"}
curl -s --unix-socket "$SOCK" http://localhost/info        # session info JSON
curl -s --unix-socket "$SOCK" http://localhost/events &    # SSE stream (ctrl-c to stop)
SSE_PID=$!
sleep 3
kill $SSE_PID

# Test mirrord ui discovers the session
./target/debug/mirrord ui --no-open --port 59286 &
UI_PID=$!
sleep 2
curl -s http://localhost:59286/api/sessions                # should list the active session

# Cleanup
kill $UI_PID $MIRRORD_PID
sleep 2

# Verify socket cleaned up
ls ~/.mirrord/sessions/*.sock 2>/dev/null && echo "FAIL: socket not cleaned" || echo "PASS: socket cleaned"
```

## Full checklist

### API server lifecycle
- [ ] `mirrord exec` with `api: true` creates socket at `~/.mirrord/sessions/<id>.sock`
- [ ] `/health` returns `{"status":"ok"}`
- [ ] `/info` returns session info JSON (session_id, target, mirrord_version, started_at)
- [ ] `/events` streams SSE events
- [ ] `/kill` (POST) triggers graceful shutdown
- [ ] Socket cleaned up after mirrord process exits
- [ ] Socket has `0600` permissions

### Event emission (via SSE stream)
- [ ] FileOp events on file operations
- [ ] DnsQuery events on DNS lookups
- [ ] IncomingRequest events on HTTP traffic (steal/mirror mode)
- [ ] HttpRequest events on outgoing HTTP
- [ ] OutgoingConnection events on outgoing TCP
- [ ] PortSubscription events on port subscribe
- [ ] EnvVar events on env var access
- [ ] LayerConnected on session start
- [ ] LayerDisconnected on session end

### mirrord ui command
- [ ] Starts and opens browser to localhost:59281
- [ ] `--no-open` starts without opening browser
- [ ] `--port` overrides default port
- [ ] `--dev` skips static files, prints message to run Vite
- [ ] Dashboard shows active sessions
- [ ] Events stream in real time per session
- [ ] Stale sockets cleaned on startup
- [ ] WebSocket endpoint streams session add/remove

### Edge cases
- [ ] `api: false` in config disables API server (no socket created)
- [ ] Multiple concurrent sessions each get their own socket
- [ ] API server crash does not affect intproxy (session continues)
- [ ] No SSE clients connected: events silently dropped, no memory growth
- [ ] Port conflict gives clear error message

## Known bugs

### Socket not cleaned up on SIGTERM
The `SocketCleanup` drop guard in `api.rs` does not run when the intproxy
process receives SIGTERM. This is because the API server runs in a
`tokio::spawn` task, and tokio cancels spawned tasks on shutdown without
running their drop guards. The socket persists at `~/.mirrord/sessions/<id>.sock`
until manually deleted or `mirrord ui` cleans it on startup.

**Fix options:**
1. Register a signal handler that removes the socket before exit
2. Use `tokio::select!` with a shutdown signal instead of relying on Drop
3. Accept the leak and rely on `mirrord ui` stale socket cleanup (current behavior)

## Integration test results (2026-03-23)

### Passing
- [x] Socket created at `~/.mirrord/sessions/<uuid>.sock` with `0600` perms
- [x] `/health` returns `{"status":"ok"}`
- [x] `/info` returns correct session JSON (target, version, config, api: true)
- [x] `mirrord ui` serves frontend HTML, CSS, JS assets (all 200)
- [x] `/api/sessions` returns `[]` when no sessions active
- [x] `--dev` mode: API works, static files not served (proxied)
- [x] `--no-open` prevents browser launch
- [x] Sessions dir created with `0700` permissions

### Blocked
- Event streaming (SSE): layer config decode error on staging prevents
  target app from running. Not a session monitor bug.
- `mirrord ui` session discovery: needs working exec session to test.
  The `mirrord ui` code correctly watches the sessions dir and connects
  to sockets, but we couldn't generate a live session with events.
