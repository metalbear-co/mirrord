#!/usr/bin/env python3
"""Summarise one or more `mirrord dump` logs: which request identifiers are
present on every hop, unique per request, and shared across hops.

Usage:
    python3 dump_stats.py [--probe VALUE] [--show-values] [--min-presence 0.9] LOG [LOG ...]

Each LOG is the captured stdout+stderr of one `mirrord dump` run; the hop name
is the file name without extension (orders.log -> orders). Only header names,
counts and short previews are printed unless --show-values is given, because
dump output contains credentials. Stdlib only.
"""

import argparse
import base64
import collections
import json
import os
import re
import sys

REQUEST_LINE = re.compile(r"^[A-Z]{3,10} \S+ HTTP/[0-9.]+")
HEADER_LINE = re.compile(r"^([A-Za-z0-9!#$%&'*+.^_`|~-]+): ?(.*)$")
TRACEPARENT = re.compile(r"^[0-9a-f]{2}-([0-9a-f]{32})-[0-9a-f]{16}-[0-9a-f]{2}$")
# Vendor trace headers: header name -> regex whose group 1 is the trace id.
TRACE_CARRIERS = {
    "x-b3-traceid": re.compile(r"^([0-9a-fA-F]{16,32})$"),
    "b3": re.compile(r"^([0-9a-fA-F]{16,32})-"),
    "x-cloud-trace-context": re.compile(r"^([0-9a-fA-F]{32})/"),
    "x-amzn-trace-id": re.compile(r"Root=([0-9a-fA-F]-[0-9a-fA-F]{8}-[0-9a-fA-F]{24})"),
    "uber-trace-id": re.compile(r"^([0-9a-fA-F]{16,32}):"),
    "x-datadog-trace-id": re.compile(r"^([0-9]+)$"),
    "sentry-trace": re.compile(r"^([0-9a-fA-F]{32})-"),
    "elastic-apm-traceparent": re.compile(r"^[0-9a-f]{2}-([0-9a-f]{32})-"),
    "ce-traceparent": re.compile(r"^[0-9a-f]{2}-([0-9a-f]{32})-"),
    "x-instana-t": re.compile(r"^([0-9a-fA-F]{16,32})$"),
    "x-honeycomb-trace": re.compile(r"trace_id=([^,;]+)"),
    "ot-tracer-traceid": re.compile(r"^([0-9a-fA-F]{16,32})$"),
}

# Headers whose values are never shown, even with --show-values.
SECRET_HEADERS = {
    "authorization", "cookie", "proxy-authorization", "x-api-key", "api-key",
    "x-amzn-oidc-data", "x-amzn-oidc-accesstoken", "x-goog-iap-jwt-assertion", "cf-access-jwt-assertion",
    "x-forwarded-access-token", "x-auth-request-access-token", "x-ms-client-principal", "newrelic",
}
# Headers carrying a JWT whose claim names are worth listing (values are hidden).
JWT_HEADERS = {
    "authorization", "x-amzn-oidc-data", "x-goog-iap-jwt-assertion", "cf-access-jwt-assertion",
    "x-forwarded-access-token", "x-auth-request-access-token", "x-amzn-oidc-accesstoken",
}
# Comma-separated key=value carriers (baggage and its older names).
MEMBER_HEADERS = {"baggage", "tracestate", "correlation-context", "jaeger-baggage", "x-datadog-tags"}
# One-header-per-key baggage conventions: prefix -> label.
MEMBER_PREFIXES = ("uberctx-", "ot-baggage-", "baggage-", "baggage_")
# Hop-by-hop or per-connection headers that cannot be identifiers.
SKIP_HEADERS = {
    "host", "content-length", "content-type", "accept", "accept-encoding", "accept-language",
    "connection", "user-agent", "te", "transfer-encoding", "cache-control", "pragma",
    "x-forwarded-for", "x-forwarded-proto", "x-forwarded-host", "x-forwarded-port",
    "x-real-ip", "x-envoy-attempt-count", "x-envoy-expected-rq-timeout-ms", "x-envoy-peer-metadata",
    "x-envoy-peer-metadata-id", "x-envoy-internal", "x-envoy-decorator-operation", "grpc-timeout",
    "content-encoding", "grpc-encoding", "grpc-accept-encoding", "x-scheme", "x-request-start",
    "mirrord-key",  # the Operator's marker for the dump session itself, not app propagation
    "via", "range", "if-none-match", "if-modified-since", "origin", "referer", "sec-fetch-mode",
    "sec-fetch-site", "sec-fetch-dest", "sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform", "priority",
}
# Requests that are health checks, not application traffic (excluded unless --include-probes).
PROBE_AGENTS = ("kube-probe/", "googlehc/", "elb-healthchecker/", "amazon-route53-health-check", "envoy/health")
PROBE_PATHS = ("/health", "/healthz", "/ready", "/readyz", "/live", "/livez", "/ping", "/status", "/-/")


def parse_log(path):
    """Return a list of requests; each is a dict name -> value (last wins)."""
    requests = []
    current = None
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            line = raw.rstrip("\r\n")
            if REQUEST_LINE.match(line):
                current = {"__line__": line, "__headers__": {}}
                requests.append(current)
                continue
            if current is None:
                continue
            if line.startswith("## ") or line == "":
                current = None
                continue
            m = HEADER_LINE.match(line)
            if m:
                name, value = m.group(1).lower(), m.group(2)
                hdrs = current["__headers__"]
                if name in hdrs:
                    # A repeated header is one header with combined values (RFC 9110 §5.3);
                    # cookies combine with "; ". Overwriting would drop a member.
                    hdrs[name] = hdrs[name] + ("; " if name == "cookie" else ", ") + value
                else:
                    hdrs[name] = value
            else:
                current = None
    return requests


def b64url_decode(s):
    s = s.replace("-", "+").replace("_", "/")
    s += "=" * ((4 - len(s) % 4) % 4)
    return base64.b64decode(s)


def jwt_claims(value):
    """Decode the payload of `Bearer <jwt>`; return a dict or None."""
    if value.lower().startswith("bearer "):
        value = value[7:].strip()
    parts = value.split(".")
    if len(parts) != 3:
        return None
    try:
        payload = json.loads(b64url_decode(parts[1]))
    except Exception:
        return None
    return payload if isinstance(payload, dict) else None


def members(value):
    """baggage / tracestate members as a dict key -> value."""
    out = {}
    for part in value.split(","):
        part = part.strip()
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = v.split(";", 1)[0].strip()
    return out


def cookie_members(value):
    out = {}
    for part in value.split(";"):
        part = part.strip()
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def sw8_trace_id(value):
    """SkyWalking sw8: field 2 is the base64 trace id."""
    parts = value.split("-")
    if len(parts) < 3:
        return None
    try:
        return b64url_decode(parts[1]).decode("utf-8", "replace")
    except Exception:
        return None


def sw8_correlation(value):
    out = {}
    for part in value.split(","):
        if ":" in part:
            k, v = part.split(":", 1)
            try:
                out[b64url_decode(k).decode("utf-8", "replace")] = b64url_decode(v).decode("utf-8", "replace")
            except Exception:
                pass
    return out


def grpc_trace_bin(value):
    """OpenCensus binary format: version(1) field-id(1) trace-id(16) ... as base64."""
    try:
        raw = b64url_decode(value.strip())
    except Exception:
        return None
    if len(raw) >= 18 and raw[0] == 0 and raw[1] == 0:
        return raw[2:18].hex()
    return None


def is_health_check(req):
    ua = req["__headers__"].get("user-agent", "").lower()
    if ua.startswith(PROBE_AGENTS):
        return True
    parts = req["__line__"].split(" ")
    path = parts[1] if len(parts) > 1 else ""
    return path.split("?", 1)[0].rstrip("/").lower() in PROBE_PATHS or path.lower().startswith(PROBE_PATHS[-1])


def identifiers(req):
    """Flatten one request into identifier-name -> value candidates."""
    ids = {}
    for name, value in req["__headers__"].items():
        if name in SKIP_HEADERS:
            continue
        if name not in SECRET_HEADERS:
            ids[name] = value
        if name == "traceparent":
            m = TRACEPARENT.match(value.strip())
            if m:
                ids["traceparent:trace-id"] = m.group(1)
        elif name in TRACE_CARRIERS:
            m = TRACE_CARRIERS[name].search(value.strip())
            if m:
                ids["%s:trace-id" % name] = m.group(1).lower()
        elif name in MEMBER_HEADERS:
            for k, v in members(value).items():
                ids["%s:%s" % (name, k)] = v
        elif name.startswith(MEMBER_PREFIXES):
            for prefix in MEMBER_PREFIXES:
                if name.startswith(prefix):
                    ids["%s*:%s" % (prefix, name[len(prefix):])] = value
                    break
        elif name == "sw8":
            tid = sw8_trace_id(value)
            if tid:
                ids["sw8:trace-id"] = tid
        elif name == "sw8-correlation":
            for k, v in sw8_correlation(value).items():
                ids["sw8-correlation:%s" % k] = v
        elif name == "grpc-trace-bin":
            tid = grpc_trace_bin(value)
            if tid:
                ids["grpc-trace-bin:trace-id"] = tid
        elif name in JWT_HEADERS and name != "authorization":
            claims = jwt_claims(value)
            if claims:
                for ck, cv in claims.items():
                    ids["%s:jwt.%s" % (name, ck)] = json.dumps(cv, sort_keys=True)
        elif name == "x-ms-client-principal":
            try:
                principal = json.loads(base64.b64decode(value))
                for c in principal.get("claims", []):
                    if c.get("typ") in ("name", "preferred_username", "email", "sub", "oid",
                                        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/nameidentifier"):
                        ids["x-ms-client-principal:%s" % c["typ"].rsplit("/", 1)[-1]] = str(c.get("val"))
            except Exception:
                pass
        elif name == "cookie":
            for k, v in cookie_members(value).items():
                ids["cookie:%s" % k] = v
                claims = jwt_claims(v)
                if claims:
                    for ck, cv in claims.items():
                        ids["cookie:%s:jwt.%s" % (k, ck)] = json.dumps(cv, sort_keys=True)
        elif name == "authorization":
            claims = jwt_claims(value)
            if claims:
                for ck, cv in claims.items():
                    ids["authorization:jwt.%s" % ck] = json.dumps(cv, sort_keys=True)
            else:
                ids["authorization:<opaque>"] = value
    return ids


def preview(value, show):
    if show:
        return value
    if len(value) <= 10:
        return "*" * len(value)
    return value[:6] + "…(%d chars)" % len(value)


def is_secret(name):
    """True for raw credential values (tokens, cookies); decoded JWT claims may preview."""
    base, _, rest = name.partition(":")
    if base in SECRET_HEADERS and ".jwt." not in ":" + rest and not rest.startswith("jwt."):
        return True
    return False


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("logs", nargs="+")
    ap.add_argument("--probe", help="value sent in the active probe; report which identifiers carry it per hop")
    ap.add_argument("--show-values", action="store_true", help="print full values (never for authorization/cookie)")
    ap.add_argument("--min-presence", type=float, default=0.9, help="presence ratio below which an identifier is not a candidate (default 0.9)")
    ap.add_argument("--include-probes", action="store_true", help="keep kube-probe / load-balancer health checks in the request base (excluded by default)")
    args = ap.parse_args()

    hops = []
    by_name = {}
    for path in args.logs:
        # "<hop>--<pod>.log" files are several replicas of one hop: merge them.
        name = os.path.splitext(os.path.basename(path))[0].split("--", 1)[0]
        reqs = parse_log(path)
        if not args.include_probes:
            reqs = [r for r in reqs if not is_health_check(r)]
        if name in by_name:
            by_name[name].extend(reqs)
        else:
            by_name[name] = reqs
            hops.append((name, by_name[name]))

    # per hop, per identifier: list of values
    per_hop = {}
    for name, reqs in hops:
        table = collections.defaultdict(list)
        for r in reqs:
            for k, v in identifiers(r).items():
                table[k].append(v)
        per_hop[name] = (len(reqs), table)

    print("Hops" + ("" if args.include_probes else "  (health checks excluded; --include-probes keeps them)"))
    for name, reqs in hops:
        print("  %-24s %5d requests" % (name, len(reqs)))
        if not reqs:
            print("    (no application requests parsed: no traffic, only health checks, TLS terminated in the pod, or non-HTTP port)")
    print()

    probe_hits = {}   # hop -> Counter(identifier -> requests carrying the probe)
    if args.probe:
        print("Probe %r" % args.probe)
        for name, reqs in hops:
            hits = collections.Counter()
            for r in reqs:
                for k, v in identifiers(r).items():
                    if args.probe in v:
                        hits[k] += 1
                for hname, hval in r["__headers__"].items():
                    if hname in SECRET_HEADERS and args.probe in hval:
                        hits[hname] += 1
            probe_hits[name] = hits
            if hits:
                print("  %-24s %s" % (name, ", ".join("%s (%d of %d requests)" % (k, c, len(reqs)) for k, c in sorted(hits.items()))))
            elif not reqs:
                print("  %-24s unknown: no application requests captured" % name)
            else:
                print("  %-24s not found in %d requests" % (name, len(reqs)))
        print()

    all_ids = sorted({k for _, (_, t) in per_hop.items() for k in t})
    print("Identifier                               " + " ".join("%-18s" % n[:18] for n, _ in hops) + " shared")
    print("  presence / distinct-per-request (U = unique per request, S = stable single value)")
    candidates = []
    for ident in all_ids:
        cells = []
        shared = None
        present_everywhere = True
        for name, _ in hops:
            total, table = per_hop[name]
            vals = table.get(ident, [])
            if total == 0:
                cells.append("%-18s" % "unknown")
                continue                      # not observed: neither evidence for nor against
            if not vals:
                cells.append("%-18s" % "-")
                present_everywhere = False
                shared = set()
                continue
            presence = len(vals) / total
            distinct = len(set(vals))
            if distinct == len(vals) and len(vals) >= 3:
                kind = "U"
            elif distinct == 1 and len(vals) >= 3:
                kind = "S"
            else:
                kind = " "
            cells.append("%-18s" % ("%3d%% %4d/%-4d %s" % (round(presence * 100), distinct, len(vals), kind)))
            if presence < args.min_presence:
                present_everywhere = False
            shared = set(vals) if shared is None else shared & set(vals)
        shared_n = len(shared or ())
        print("  %-40s %s %5d" % (ident[:40], " ".join(cells), shared_n))
        if present_everywhere and len(hops) > 1 and shared_n > 0:
            candidates.append((ident, shared_n))
    print()

    observed = [name for name, reqs in hops if reqs]
    unknown = [name for name, reqs in hops if not reqs]
    if unknown:
        print("Unknown hops (no application requests captured; not evidence of a gap): " + ", ".join(unknown))
        print()

    if args.probe:
        # Probe mode: a candidate carried the probe value at every observed hop, whatever else was captured.
        print("Candidates carrying the probe at every observed hop (%s):" % ", ".join(observed))
        all_hit = sorted({k for h in observed for k in probe_hits.get(h, ())})
        found = False
        for ident in all_hit:
            if all(probe_hits.get(h, collections.Counter())[ident] > 0 for h in observed):
                found = True
                print("  %-40s %s" % (ident, "  ".join("%s %d/%d" % (h, probe_hits[h][ident], len(dict(hops)[h])) for h in observed)))
        if not found:
            print("  none carried the probe to every observed hop. Hops where it stops:")
            for h in observed:
                if not probe_hits.get(h):
                    print("    %s" % h)
    elif len(hops) > 1:
        print("Candidates present on every observed hop (presence >= %d%%) with values shared across them:" % round(args.min_presence * 100))
        if not candidates:
            print("  none meets the threshold on every observed hop. Read the per-hop columns before calling this a gap; background traffic lowers presence.")
        for ident, n in candidates:
            sample = ""
            if not is_secret(ident):
                first_hop = hops[0][0]
                vals = per_hop[first_hop][1].get(ident, [])
                if vals:
                    sample = "  e.g. " + preview(vals[0], args.show_values)
            print("  %-40s %5d shared value(s)%s" % (ident, n, sample))
    return 0


if __name__ == "__main__":
    sys.exit(main())
