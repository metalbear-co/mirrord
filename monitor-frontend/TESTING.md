# Session Monitor Testing

Test checklist for the combined session monitor feature. Run these before updating the combined PR.

## Rust

### Compile checks
- [ ] `cargo check -p mirrord-intproxy --keep-going`
- [ ] `MIRRORD_LAYER_FILE_MACOS_ARM64=/dev/null cargo check -p mirrord --keep-going`

### Tests
- [ ] `cargo test -p mirrord-intproxy` (37+ tests)

### Lint
- [ ] `cargo clippy -p mirrord-intproxy` (0 warnings)
- [ ] `cargo fmt -- --check` (no diff)

## Frontend

### Build
- [ ] `cd monitor-frontend && npm ci && npm run build`

### Type check
- [ ] `cd monitor-frontend && npx tsc --noEmit`

## Integration (manual)

### API server lifecycle
- [ ] Run `mirrord exec` with `api: true`, verify socket appears at `~/.mirrord/sessions/<id>.sock`
- [ ] `curl --unix-socket ~/.mirrord/sessions/<id>.sock http://localhost/health` returns `{"status":"ok"}`
- [ ] `curl --unix-socket ~/.mirrord/sessions/<id>.sock http://localhost/info` returns session info JSON
- [ ] Socket is cleaned up after mirrord process exits

### Event emission
- [ ] SSE stream (`/events`) emits FileOp events when target app does file operations
- [ ] SSE stream emits DnsQuery events on DNS lookups
- [ ] SSE stream emits IncomingRequest events on HTTP traffic (steal/mirror mode)
- [ ] SSE stream emits OutgoingConnection events on outgoing TCP
- [ ] SSE stream emits LayerConnected on session start

### mirrord ui command
- [ ] `mirrord ui` starts and opens browser to localhost:59281
- [ ] Dashboard shows active sessions
- [ ] Events stream in real time per session
- [ ] `mirrord ui --no-open` starts without opening browser
- [ ] `mirrord ui --dev` proxies to Vite dev server (port 5173)
- [ ] Stale sockets are cleaned on startup

### Edge cases
- [ ] `api: false` in config disables the API server (no socket created)
- [ ] Multiple concurrent mirrord sessions each get their own socket
- [ ] API server crash does not affect intproxy (session continues working)
- [ ] No SSE clients connected: events are silently dropped, no memory growth
