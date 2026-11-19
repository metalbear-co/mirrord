# Filter recipes

How to turn a discovered identifier into `feature.network.incoming.http_filter`. Every block below validates against the mirrord config schema.

Rules that apply to all of them:

- **Headers are matched as `name: value` strings, one header at a time.** Regex filters get `(?i)` prepended, so they are case-insensitive. jq filters receive each header as a JSON string such as `"authorization: Bearer eyJ..."` with the **name in lowercase**, and the request matches when the program outputs `true` for any header. A program that outputs nothing, or errors, does not match.
- **`http_filter` holds exactly one of** `header_filter`, `header_filter_jq`, `path_filter`, `method_filter`, `body_filter`, `all_of`, `any_of`, plus the optional `ports` list. Inside `all_of` / `any_of`, each entry is one of `{ "header" }`, `{ "query" }` (jq on headers), `{ "path" }`, `{ "method" }`, or a `{ "body": "json", "query", "matches" }` body filter.
- **Use `{{ key }}` for developer-chosen values.** mirrord fills it from `--key`, `MIRRORD_KEY`, or `"key"` in the config, so one committed config serves every developer. Identity-bound values (a claim, a tenant) are per developer too: keep them in `{{ key }}` or an env var via `{{ get_env(name="MY_TENANT") }}`, never a literal in a shared file.
- **`{{ key }}` is pasted into a regex verbatim.** Keep keys to letters, digits, `-` and `_`. A value with other punctuation (an email with `+` or `.`, a tenant slug with `/`) belongs in a jq equality (`. == "{{ key }}"`), never in a regex, or the developer's own value fails to match its filter.
- **Match whole members, not substrings.** `.*mirrord-session={{ key }}.*` also matches `mirrord-session={{ key }}2` and would steal another developer's requests. The patterns below require a member boundary before the name (start or `,`) and after the value (`,`, `;` or end). Same for cookies (`;`).
- **Set `ports`** to the target's HTTP ports when the pod also exposes non-HTTP ports (metrics, gRPC health, a debugger), otherwise mirrord applies the filter to every port it subscribes to.
- **Start in `mirror` mode** to verify a filter; switch to `steal` only when the local process must be the one responding.
- The jq engine is jaq (a jq reimplementation), with a 500 ms time limit per evaluation (`agent.jaq_time_limit`). Keep programs small and guard them with `select(startswith("<name>: "))` so only the header you care about is decoded. Test a program locally with `jq -n --arg h 'authorization: Bearer ...' '$h | <program>'` before putting it in the config; the behaviours used here (`select`, `sub`, `gsub`, `split`, `ltrimstr`, `@base64d`, `fromjson`, `try`/`catch`, `test`) match between jq 1.7 and jaq.

## Baggage member (recommended)

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter": "^baggage:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*([,;].*)?$",
  "ports": [8080]
}
```

Reads: the header starts with `baggage:`, then optionally other members ending in a comma, then `mirrord-session=<key>`, then either the end, another member (`,`) or a member property (`;`). `tracestate` has no `;` properties: `"^tracestate:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*(,.*)?$"`.

## Correlation-id header, exact value

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter": "^x-request-id: {{ key }}$"
}
```

For headers whose value is a prefix or contains other parts (`x-amzn-trace-id: Root=1-...;Self=...`), anchor on the part the developer controls: `"^x-amzn-trace-id: .*{{ key }}"`.

## `traceparent` trace-id

`{{ key }}` must be the 32-hex-character trace-id the developer sends; the span-id changes at every hop and must not be in the pattern.

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter": "^traceparent: 00-{{ key }}-"
}
```

## Other trace ids (B3, Google, X-Ray, Jaeger)

Anchor on the trace-id part only; the span part changes every hop.

```json mirrord=feature.network.incoming.http_filter
{
  "any_of": [
    { "header": "^x-b3-traceid: {{ key }}$" },
    { "header": "^b3: {{ key }}-" },
    { "header": "^x-cloud-trace-context: {{ key }}/" },
    { "header": "^x-amzn-trace-id: .*Root={{ key }}(;|$)" },
    { "header": "^uber-trace-id: {{ key }}:" }
  ]
}
```

Keep only the entry for the header the dump showed. For X-Ray, `{{ key }}` is the full `Root` value (`1-<8 hex>-<24 hex>`).

## Tenant or user id header

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter": "^x-tenant-id: {{ key }}$"
}
```

When the tenant lives in a JWT claim instead, use the JWT recipe below with `.tenant_id == "{{ key }}"`.

## JWT claim in `authorization: Bearer <jwt>`

Decodes the JWT payload and matches when the `sub` claim equals the key. The `select` guard keeps the program from running on other headers; `try ... catch false` turns malformed tokens into a non-match instead of an agent error log.

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter_jq": "try (select(startswith(\"authorization: \")) | sub(\"^authorization: [Bb]earer\\\\s+\"; \"\") | split(\".\") | select(length == 3) | .[1] | gsub(\"-\"; \"+\") | gsub(\"_\"; \"/\") | . as $s | ($s | length % 4) as $r | (if $r == 0 then $s else $s + (\"=\" * (4 - $r)) end) | @base64d | fromjson | .sub == \"{{ key }}\") catch false"
}
```

Swap `.sub == "{{ key }}"` for any claim: `.email == "{{ key }}"`, `.tenant_id == "{{ key }}"`, or a regex `.email | test("@{{ key }}$")`. For a JWT carried in a cookie, replace the first two steps with `select(startswith("cookie: ")) | ltrimstr("cookie: ") | split("; ") | map(select(startswith("id_token="))) | .[0] | ltrimstr("id_token=")`.

## Baggage under an older or vendor name

Clusters on Jaeger, OpenTracing, old Sleuth, or old .NET carry user-defined keys under these names instead of `baggage`. Match the member the same way.

```json mirrord=feature.network.incoming.http_filter
{
  "any_of": [
    { "header": "^uberctx-mirrord-session: {{ key }}$" },
    { "header": "^ot-baggage-mirrord-session: {{ key }}$" },
    { "header": "^baggage-mirrord-session: {{ key }}$" },
    { "header": "^correlation-context:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*([,;].*)?$" },
    { "header": "^jaeger-baggage:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*(,.*)?$" }
  ]
}
```

## Identity injected by an auth proxy

Plain-value headers from oauth2-proxy, IAP, ALB, Cloudflare Access. Emails contain regex metacharacters (`.`, `+`), so compare them as strings in jq instead of a regex; keep only the entry for the header the dump showed.

```json mirrord=feature.network.incoming.http_filter
{
  "any_of": [
    { "query": "select(startswith(\"x-auth-request-email: \")) | ltrimstr(\"x-auth-request-email: \") | ascii_downcase == (\"{{ key }}\" | ascii_downcase)" },
    { "query": "select(startswith(\"x-forwarded-email: \")) | ltrimstr(\"x-forwarded-email: \") | ascii_downcase == (\"{{ key }}\" | ascii_downcase)" },
    { "query": "select(startswith(\"x-goog-authenticated-user-email: \")) | ltrimstr(\"x-goog-authenticated-user-email: accounts.google.com:\") | ascii_downcase == (\"{{ key }}\" | ascii_downcase)" },
    { "query": "select(startswith(\"x-amzn-oidc-identity: \")) | ltrimstr(\"x-amzn-oidc-identity: \") | . == \"{{ key }}\"" },
    { "query": "select(startswith(\"cf-access-authenticated-user-email: \")) | ltrimstr(\"cf-access-authenticated-user-email: \") | ascii_downcase == (\"{{ key }}\" | ascii_downcase)" }
  ]
}
```

Signed-JWT headers (`x-amzn-oidc-data`, `x-goog-iap-jwt-assertion`, `cf-access-jwt-assertion`) hold the same claims as a bearer token; use the JWT recipe with the header name swapped and no `Bearer` prefix:

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter_jq": "try (select(startswith(\"x-amzn-oidc-data: \")) | ltrimstr(\"x-amzn-oidc-data: \") | split(\".\") | select(length == 3) | .[1] | gsub(\"-\"; \"+\") | gsub(\"_\"; \"/\") | . as $s | ($s | length % 4) as $r | (if $r == 0 then $s else $s + (\"=\" * (4 - $r)) end) | @base64d | fromjson | .email == \"{{ key }}\") catch false"
}
```

## A routing or lane header the team already propagates

If the cluster already routes canaries or sandboxes by header, reuse that header and let `{{ key }}` be the developer's lane:

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter": "^x-env: {{ key }}$"
}
```

## CloudEvents in structured mode

Binary mode puts the trace in `ce-traceparent` (use the `traceparent` recipe with the header name changed). Structured mode puts it in the JSON body:

```json mirrord=feature.network.incoming.http_filter
{
  "body_filter": {
    "body": "json",
    "query": "$.traceparent",
    "matches": "^00-{{ key }}-"
  }
}
```

## Cookie value

Regex is enough when the cookie value is the identifier:

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter": "^cookie:\\s*(.*;\\s*)?session_id={{ key }}\\s*(;.*)?$"
}
```

jq, when you want exact parsing of the cookie list:

```json mirrord=feature.network.incoming.http_filter
{
  "header_filter_jq": "try (select(startswith(\"cookie: \")) | ltrimstr(\"cookie: \") | split(\"; \") | map(select(startswith(\"session_id=\"))) | .[0] | ltrimstr(\"session_id=\") | . == \"{{ key }}\") catch false"
}
```

## Identity header plus a route, combined

`all_of` narrows to one developer's tenant **and** one endpoint, which keeps a steal session from taking the tenant's health checks or unrelated routes.

```json mirrord=feature.network.incoming.http_filter
{
  "all_of": [
    { "header": "^x-tenant-id: {{ key }}$" },
    { "path": "^/v1/orders" },
    { "method": "POST" }
  ]
}
```

A jq entry inside a combinator uses `query`:

```json mirrord=feature.network.incoming.http_filter
{
  "all_of": [
    { "query": "try (select(startswith(\"authorization: \")) | sub(\"^authorization: [Bb]earer\\\\s+\"; \"\") | split(\".\") | select(length == 3) | .[1] | gsub(\"-\"; \"+\") | gsub(\"_\"; \"/\") | . as $s | ($s | length % 4) as $r | (if $r == 0 then $s else $s + (\"=\" * (4 - $r)) end) | @base64d | fromjson | .tenant_id == \"{{ key }}\") catch false" },
    { "path": "^(?!/health)" }
  ]
}
```

## Either of two carriers

When some clients send `baggage` and others `tracestate`:

```json mirrord=feature.network.incoming.http_filter
{
  "any_of": [
    { "header": "^baggage:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*([,;].*)?$" },
    { "header": "^tracestate:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*(,.*)?$" }
  ]
}
```

## Path segment or query parameter

`path_filter` is matched against the path including the query string.

```json mirrord=feature.network.incoming.http_filter
{
  "path_filter": "^/tenants/{{ key }}/"
}
```

```json mirrord=feature.network.incoming.http_filter
{
  "path_filter": "[?&]session={{ key }}(&|$)"
}
```

## JSON body field

`query` is a JSONPath (RFC 9535) expression; `matches` is a regex applied to each result. Only requests with a JSON body can match, and the agent buffers the body to evaluate it.

```json mirrord=feature.network.incoming.http_filter
{
  "body_filter": {
    "body": "json",
    "query": "$.metadata.session",
    "matches": "^{{ key }}$"
  }
}
```

## Complete config for verification

Mirror mode, filter on the discovered identifier, ports pinned. Health checks carry no identifier, so the filter already excludes them; don't add `^user-agent: (?!kube-probe)` to an `all_of`, it requires a `user-agent` header and drops internal requests sent without one. Only a path-only filter needs a probe exclusion, as `{ "path": "^(?!/health)" }`. Run it with `mirrord exec -f .mirrord/mirrord.json --key alice -- <command>`.

```json mirrord
{
  "target": {
    "path": "deployment/orders",
    "namespace": "staging"
  },
  "feature": {
    "network": {
      "incoming": {
        "mode": "mirror",
        "ports": [8080],
        "http_filter": {
          "header_filter": "^baggage:\\s*(.*,\\s*)?mirrord-session={{ key }}\\s*([,;].*)?$",
          "ports": [8080]
        }
      }
    }
  }
}
```

Switch `"mode"` to `"steal"` once the mirror run shows only the developer's requests arriving locally.
