# Running and reading `mirrord dump`

`mirrord dump` starts a mirror session on a target and prints every incoming connection and request to stdout. It only mirrors (copies) traffic: the target keeps serving every request, nothing is stolen, and the only cluster change is a short-lived mirrord agent pod (or an ephemeral container with `-e`) that goes away when the dump stops.

```bash
mirrord dump -t deployment/orders -n staging -p 8080
mirrord dump -t deployment/orders -n staging            # 3.191.0+: attaches to every containerPort in the pod spec
mirrord dump -t deployment/orders -n staging --context staging-cluster
```

A `deployment/` (or `service/`, `rollout/`...) target attaches the agent to **one pod** of that workload. For a hop with several replicas, dump each pod (`-t pod/<name>`) into its own log, named `<hop>--<pod>.log` so `dump_stats.py` merges them into one hop; otherwise a probe routed to another replica looks like a propagation gap. Target syntax is the same as `mirrord exec`: `deployment/`, `pod/`, `statefulset/`, `rollout/`, `service/`, `label/k=v`, optionally `/container/<name>`. Targetless is not supported. If port detection fails (`couldn't detect ports on target`), the containers declare no `ports:`; read the Service's `targetPort` and pass `-p`.

Traffic goes to stdout. Progress (`Listening for traffic... Press Ctrl+C to stop`) is drawn as a spinner and **disappears when stdout is not a terminal**, so set `MIRRORD_PROGRESS_MODE=dumb` to get it as plain lines, and capture both streams into a file outside the repo:

```bash
export MIRRORD_PROGRESS_MODE=dumb
export MIRRORD_CHECK_VERSION=false     # keeps the update nag (which suggests a piped install script) out of the logs
mirrord dump -t deployment/orders -n staging > "$DUMP_DIR/orders.log" 2>&1
```

## Output format

Each HTTP request the agent parses prints as a marker line, the request line, the headers, then the body frames. Header names print in lowercase.

```text
## New TCP connection established: Connection ID 3 from 10.42.0.17:48122 to 10.42.1.8:8080
## New HTTP request received: Request ID [3:0] to port 8080
POST /v1/orders HTTP/1.1
host: orders.staging.svc.cluster.local
content-type: application/json
authorization: Bearer eyJhbGciOi...
traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
baggage: mirrord-session=alice,sentry-trace_id=4bf92f35
x-request-id: 9c1f6a2e-3d4b-4a8e-9f0b-1b2c3d4e5f60
## Request [3:0] body: 58 bytes
{"customer_id":"c-1842","items":[{"sku":"A1","qty":2}]}
## Request ID [3:0] finished
## Connection ID 3 closed
```

Variants you will see:

- Through the Operator the marker reads `## New HTTP request received: Request ID [3:0] from <src> to <dst>` and `HTTPS (ALPN=..., SNI=...)` when the mesh handed over TLS metadata.
- Chunked bodies print as `## Request ID [3:0] frame: data (N bytes)` lines, and trailers as `frame: trailers`.
- Traffic the agent does not parse as HTTP prints as `## Connection ID 3: 93 bytes` followed by `Data:` and the raw payload. If that payload starts with a request line, it is still HTTP: read the headers from it.
- `## New TLS connection established ... (ALPN=..., SNI=...)` followed by `Data (hex):` means the pod terminates TLS itself. No headers are visible; see the catalog's "Hops `mirrord dump` cannot show".
- Through the Operator every mirrored request also carries `mirrord-key: <id>`: the Operator's marker for **your dump session**, not something the app propagates. Ignore it (`dump_stats.py` does).
- Health checks dominate quiet services: `user-agent: kube-probe/...` and `GoogleHC/1.0` (GKE load balancer) requests to `/health`. They are why the stats script reports presence per header rather than raw counts, and why filters exclude `kube-probe`.
- Older CLIs print the same information without the `##` prefix.

gRPC calls appear as HTTP/2 `POST /<package.Service>/<Method>` requests with metadata as headers and a protobuf body (printed as hex).

## Active probe (preferred)

Dump every hop at once, send one request you can recognise at the edge, then look for its value at each hop.

```bash
export MIRRORD_PROGRESS_MODE=dumb              # plain progress lines; the spinner prints nothing when redirected
export MIRRORD_CHECK_VERSION=false
DUMP_DIR=$(mktemp -d)
NS=staging
HOPS="gateway orders inventory billing"      # one deployment per hop, edge first

PIDS=""
trap 'kill $PIDS 2>/dev/null; wait $PIDS 2>/dev/null' EXIT INT TERM   # never leave dumps capturing
for h in $HOPS; do
  # one dump per pod so a multi-replica hop is fully observed; logs named <hop>--<pod>.log merge in dump_stats.py
  for pod in $(kubectl get pods -n "$NS" -o name --field-selector=status.phase=Running | grep "^pod/$h-"); do
    mirrord dump -t "$pod" -n "$NS" > "$DUMP_DIR/$h--${pod#pod/}.log" 2>&1 &
    PIDS="$PIDS $!"
  done
done
# wait until every dump is subscribed (or give up after 90s and look at the log)
for log in "$DUMP_DIR"/*.log; do
  n=0
  until grep -q "Listening for traffic" "$log"; do
    sleep 2; n=$((n+2))
    if [ $n -ge 90 ]; then echo "$(basename "$log"): not subscribed after 90s"; tail -5 "$log"; break; fi
  done
done

PROBE="probe-$(date +%s)-$RANDOM"
TRACE_ID=$(printf '%032x' "$RANDOM$RANDOM$RANDOM")
curl -sS -m 20 -o /dev/null "$EDGE_URL/v1/orders" \
  -H "baggage: mirrord-session=$PROBE" \
  -H "tracestate: mirrord-session=$PROBE" \
  -H "x-request-id: $PROBE" \
  -H "traceparent: 00-$TRACE_ID-00f067aa0ba902b7-01" \
  -H "authorization: Bearer $DEV_TOKEN"        # the developer's own test user, if the flow needs auth

sleep 5
kill $PIDS 2>/dev/null; wait $PIDS 2>/dev/null     # by PID: job numbers (%1) don't work in non-interactive shells

python3 scripts/dump_stats.py --probe "$PROBE" "$DUMP_DIR"/*.log
```

Read the result per hop:

- A header line containing `$PROBE` means that header is **present and controllable** at this hop. The headers that carry it at the last hop are the candidates.
- One edge request usually produces **several** downstream requests (a stock check and a reserve, say). Count them: the probe value should be on every one of them, and `dump_stats.py` reports the ratio.
- The marker at hop 1 shows `$PROBE` under `x-request-id` only if the ingress preserved it. If the value differs, the ingress overwrote it.
- `$TRACE_ID` appearing inside `traceparent` at every hop means tracecontext propagation works end to end. If it is present at hop 1 and missing later, that service starts a new trace.
- Nothing at a hop while the request did reach it (you see its request without the probe) means every candidate was dropped there.
- Nothing at a hop and **no request at all** means you did not observe it: a replica you did not dump, a path that did not fan out there, or a queue in between. Record it as unknown and probe again, never as a gap.

Reach the edge the way real clients do: the public ingress URL, or `kubectl port-forward svc/<edge> 8080:80` when there is no external route. Pick endpoints that fan out through every hop; run several probes if one path does not cover them all. For a browser-driven flow, the mirrord browser extension can inject the same headers.

## Passive observation

When you cannot send requests, dump each hop for a window (a few minutes of real traffic) and let the stats tell you what is already there:

```bash
timeout 180 mirrord dump -t deployment/orders -n "$NS" > "$DUMP_DIR/orders.log" 2>&1
python3 scripts/dump_stats.py "$DUMP_DIR"/*.log
```

`dump_stats.py` (stdlib only) prints, per hop, the request count and for each header: presence, distinct values, and whether the value is unique per request or stable; it decodes JWT claim names from `authorization: Bearer` and `baggage`/`tracestate` member keys, and reports which values appear on every hop. With `--probe <value>` the candidates are the identifiers that carried the probe at every observed hop, independent of how much other traffic was captured; without it, candidates come from presence across all captured requests. Hops with no application requests are reported as unknown in both modes. It prints names, counts and short previews only; `--show-values` is needed to see full values, and you should not need it.

Without the script, the same questions by hand:

```bash
# header names and how often they appear (one line per header per request)
grep -E '^[a-z0-9!#$%&*+.^_`|~-]+: ' orders.log | cut -d: -f1 | sort | uniq -c | sort -rn
# number of requests
grep -c '^## New HTTP request received' orders.log
# distinct values of one header (unique per request when it equals the request count)
grep -i '^x-request-id: ' orders.log | sort -u | wc -l
# values of one header that appear in two hops
comm -12 <(grep -i '^x-request-id: ' gateway.log | sort -u) <(grep -i '^x-request-id: ' orders.log | sort -u)
```

## Handling what the dump contains

Dump output carries whatever clients send: bearer tokens, cookies, API keys, personal data in bodies.

- Write logs to a temp directory outside the repository and delete them when done (`rm -rf "$DUMP_DIR"`).
- Quote header **names** and value **shapes** in the report (`authorization: Bearer <jwt, claims: sub, email, tenant_id>`), never full values.
- Never put a real token, cookie, or trace id from a dump into `mirrord.json`. Filters match a claim or a developer-chosen key.
- Don't paste dump excerpts into tickets, chat, or commits.
