# Minimal propagation layer

Use these when a service has no stack that can propagate baggage, or its stack misses a hop. Default to the **OpenTelemetry API with the W3C baggage propagator and no exporter**. That needs no collector or tracing setup, and it stays compatible if the team adds tracing later. Adapt the snippets to the frameworks and clients actually in the repo, and put each one at a single boundary (middleware, interceptor, producer/consumer wrapper), not at call sites.

The contract every snippet implements:

1. **Ingress:** read `baggage` from the request/message and put it in the context of this request/message.
2. **Egress:** write the context's baggage onto every outgoing request/message.
3. **Merge, don't replace:** the propagator serialises all members; never write a `baggage` value containing only `mirrord-session`.
4. **Keep the existing propagator.** The global-propagator snippets below are for services that configure none. If the service already sets one (B3, X-Ray, Jaeger, Datadog, custom), add the baggage propagator to that existing call or to `OTEL_PROPAGATORS` instead. A second `Set...Propagator` call replaces the first and breaks the context the service already forwards.
5. **Internal destinations only.** Client-side injection runs only for internal hosts. Each language section has an `isInternal` check; fill its suffix list from the hosts the service really calls (cluster DNS, internal domains), and strip `baggage` on clients that call external APIs.

## Go

```go
import (
    "go.opentelemetry.io/otel"
    "go.opentelemetry.io/otel/propagation"
    "go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
    "go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
)

func init() {
    // Only when the service sets no propagator. Otherwise add propagation.Baggage{}
    // to its existing NewCompositeTextMapPropagator call.
    // No TracerProvider is needed for baggage-only propagation.
    otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator(
        propagation.TraceContext{}, propagation.Baggage{}))
}

var internalSuffixes = []string{".svc.cluster.local", ".internal.example.com"} // fill from real hosts

func isInternal(host string) bool {
    h := strings.ToLower(strings.Split(host, ":")[0])
    if !strings.Contains(h, ".") { return true } // bare service name resolved in-cluster
    for _, s := range internalSuffixes { if strings.HasSuffix(h, s) { return true } }
    return false
}

// internalOnly injects context for internal hosts and strips baggage for everything else.
type internalOnly struct{ internal, external http.RoundTripper }

func (t internalOnly) RoundTrip(r *http.Request) (*http.Response, error) {
    if isInternal(r.URL.Host) { return t.internal.RoundTrip(r) }
    r = r.Clone(r.Context()); r.Header.Del("baggage")
    return t.external.RoundTrip(r)
}

// HTTP: wrap the server handler and the client transport.
handler = otelhttp.NewHandler(mux, "server")
client := &http.Client{Transport: internalOnly{
    internal: otelhttp.NewTransport(http.DefaultTransport), external: http.DefaultTransport}}
// Outgoing requests must be built with the request's ctx: http.NewRequestWithContext(ctx, ...)

// gRPC server
grpc.NewServer(grpc.StatsHandler(otelgrpc.NewServerHandler()))

// gRPC client: a ClientConn has one fixed target, so classify it once when dialing.
// Internal targets get the propagating handler. External targets get no handler, plus
// interceptors that remove any baggage metadata the app set by hand.
func grpcClientOpts(target string) []grpc.DialOption {
    host := target
    if i := strings.Index(host, "///"); i >= 0 { host = host[i+3:] } // "dns:///svc.ns.svc.cluster.local:443"
    if isInternal(host) {
        return []grpc.DialOption{grpc.WithStatsHandler(otelgrpc.NewClientHandler())}
    }
    return []grpc.DialOption{
        grpc.WithUnaryInterceptor(func(ctx context.Context, method string, req, reply any,
            cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
            return invoker(stripBaggage(ctx), method, req, reply, cc, opts...)
        }),
        grpc.WithStreamInterceptor(func(ctx context.Context, desc *grpc.StreamDesc,
            cc *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
            return streamer(stripBaggage(ctx), desc, cc, method, opts...)
        }),
    }
}

func stripBaggage(ctx context.Context) context.Context {
    md, ok := metadata.FromOutgoingContext(ctx)
    if !ok { return ctx }
    md = md.Copy(); md.Delete("baggage")
    return metadata.NewOutgoingContext(ctx, md)
}

conn, err := grpc.NewClient(target, append(grpcClientOpts(target), creds)...)
```

Build every client connection through `grpcClientOpts` so each one is classified. Don't add `otelgrpc.NewClientHandler()` to clients directly.

Kafka (any client) — a carrier over the client's header slice. Example for `segmentio/kafka-go`:

```go
type kafkaHeaders struct{ h *[]kafka.Header }

func (c kafkaHeaders) Get(k string) string {
    for _, h := range *c.h { if h.Key == k { return string(h.Value) } }
    return ""
}
func (c kafkaHeaders) Set(k, v string) {
    for i, h := range *c.h { if h.Key == k { (*c.h)[i].Value = []byte(v); return } }
    *c.h = append(*c.h, kafka.Header{Key: k, Value: []byte(v)})
}
func (c kafkaHeaders) Keys() []string {
    ks := make([]string, 0, len(*c.h)); for _, h := range *c.h { ks = append(ks, h.Key) }; return ks
}

// produce
otel.GetTextMapPropagator().Inject(ctx, kafkaHeaders{&msg.Headers})
// consume — per message
ctx := otel.GetTextMapPropagator().Extract(context.Background(), kafkaHeaders{&m.Headers})
handle(ctx, m)
```

SQS (`aws-sdk-go-v2`):

```go
// produce — add only baggage, and only if it fits SQS's 10-attribute limit
const sqsMaxAttributes = 10
carrier := propagation.MapCarrier{}
otel.GetTextMapPropagator().Inject(ctx, carrier)
if b := carrier.Get("baggage"); b != "" {
    _, exists := input.MessageAttributes["baggage"]
    if exists || len(input.MessageAttributes) < sqsMaxAttributes {
        if input.MessageAttributes == nil { input.MessageAttributes = map[string]types.MessageAttributeValue{} }
        input.MessageAttributes["baggage"] = types.MessageAttributeValue{DataType: aws.String("String"), StringValue: aws.String(b)}
    } else {
        log.Printf("sqs: %d attributes already set, sending without baggage", len(input.MessageAttributes))
    }
}
// consume — request the attributes, then extract per message
recv.MessageAttributeNames = []string{"All"}
carrier := propagation.MapCarrier{}
for k, v := range m.MessageAttributes { if v.StringValue != nil { carrier[k] = *v.StringValue } }
ctx := otel.GetTextMapPropagator().Extract(context.Background(), carrier)
```

Pub/Sub (`cloud.google.com/go/pubsub`): the same `MapCarrier` pattern against `msg.Attributes` (publish) and `m.Attributes` inside the `Receive` callback (consume). RabbitMQ (`amqp091-go`): the same against `amqp.Table` in `Publishing.Headers` / `Delivery.Headers` (values are `interface{}` — store strings).

Goroutines: pass `ctx` into anything started from a handler. `go work()` without `ctx` drops baggage.

## Python

```python
import urllib.parse

from opentelemetry import baggage, context
from opentelemetry.propagate import inject, extract, set_global_textmap
from opentelemetry.propagators.composite import CompositePropagator
from opentelemetry.trace.propagation.tracecontext import TraceContextTextMapPropagator
from opentelemetry.baggage.propagation import W3CBaggagePropagator

# Only when the service configures no propagator; otherwise set OTEL_PROPAGATORS
# (e.g. "b3,baggage") or add W3CBaggagePropagator to its existing composite.
set_global_textmap(CompositePropagator([TraceContextTextMapPropagator(), W3CBaggagePropagator()]))

INTERNAL_SUFFIXES = (".svc.cluster.local", ".internal.example.com")  # fill from real hosts

def is_internal(url: str) -> bool:
    host = (urllib.parse.urlsplit(url).hostname or "").lower()
    return "." not in host or host.endswith(INTERNAL_SUFFIXES)
```

HTTP/gRPC: prefer the instrumentation packages (`opentelemetry-instrumentation-flask|django|fastapi|requests|httpx|grpc`) — they work without an exporter. They inject into every outgoing request, so strip `baggage` on external calls (e.g. a `requests` session or `httpx` event hook for external hosts that deletes the header). Manual fallback:

```python
# server middleware
token = context.attach(extract(request.headers))
try: ...handle...
finally: context.detach(token)

# client — internal hosts only
headers = {}
if is_internal(url): inject(headers)
requests.get(url, headers=headers)
```

Kafka (`confluent_kafka`):

```python
# produce — headers as a list of (str, bytes)
carrier = {}; inject(carrier)
producer.produce(topic, value, headers=[(k, v.encode()) for k, v in carrier.items()])

# consume — per message
msg = consumer.poll(1.0)
carrier = {k: v.decode() for k, v in (msg.headers() or [])}
token = context.attach(extract(carrier))
try: handle(msg)
finally: context.detach(token)
```

SQS (`boto3`):

```python
SQS_MAX_ATTRIBUTES = 10
carrier = {}; inject(carrier)
attrs = dict(existing)
if "baggage" in carrier:
    if "baggage" in attrs or len(attrs) < SQS_MAX_ATTRIBUTES:
        attrs["baggage"] = {"DataType": "String", "StringValue": carrier["baggage"]}
    else:
        log.warning("sqs: %d attributes already set, sending without baggage", len(attrs))
sqs.send_message(QueueUrl=url, MessageBody=body, MessageAttributes=attrs)

resp = sqs.receive_message(QueueUrl=url, MessageAttributeNames=["All"])
for m in resp.get("Messages", []):
    carrier = {k: v["StringValue"] for k, v in m.get("MessageAttributes", {}).items() if "StringValue" in v}
    token = context.attach(extract(carrier))
    try: handle(m)
    finally: context.detach(token)
```

Pub/Sub: `publisher.publish(topic, data, **carrier)` (attributes are kwargs); in the subscriber callback, `extract(dict(message.attributes))`. RabbitMQ (`pika`): `pika.BasicProperties(headers=carrier)` / `extract(properties.headers or {})`.

Celery / thread pools: `contextvars` don't cross processes. For Celery, `opentelemetry-instrumentation-celery` carries context in task headers. For `ThreadPoolExecutor`, submit with `contextvars.copy_context().run`.

## Node.js / TypeScript

```ts
import { propagation, context } from '@opentelemetry/api';
import { W3CBaggagePropagator, W3CTraceContextPropagator, CompositePropagator } from '@opentelemetry/core';
import { AsyncLocalStorageContextManager } from '@opentelemetry/context-async-hooks';

context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());
// Only when the service configures no propagator; otherwise add W3CBaggagePropagator
// to its existing CompositePropagator or set OTEL_PROPAGATORS.
propagation.setGlobalPropagator(new CompositePropagator({
  propagators: [new W3CTraceContextPropagator(), new W3CBaggagePropagator()],
}));

const INTERNAL_SUFFIXES = ['.svc.cluster.local', '.internal.example.com']; // fill from real hosts
const isInternal = (url: string) => {
  const host = new URL(url).hostname.toLowerCase();
  return !host.includes('.') || INTERNAL_SUFFIXES.some((s) => host.endsWith(s));
};
```

HTTP: `@opentelemetry/instrumentation-http` (+ `-express` / `-fastify` / `-undici`) registered with `registerInstrumentations` — no exporter needed. Those inject into every outgoing request; pass `ignoreOutgoingRequestHook` (http) / `ignoreRequestHook` (undici) returning `true` for non-internal hosts so no context is injected there. Manual fallback:

```ts
// server (express)
app.use((req, _res, next) => context.with(propagation.extract(context.active(), req.headers), next));
// client
const headers: Record<string, string> = {};
if (isInternal(url)) propagation.inject(context.active(), headers);
await fetch(url, { headers });
```

Kafka (`kafkajs`):

```ts
// produce
const headers: Record<string, string> = {};
propagation.inject(context.active(), headers);
await producer.send({ topic, messages: [{ value, headers }] });

// consume — per message (eachBatch: do this inside the loop over batch.messages)
await consumer.run({
  eachMessage: async ({ message }) => {
    const carrier = Object.fromEntries(
      Object.entries(message.headers ?? {}).map(([k, v]) => [k, v?.toString() ?? '']));
    await context.with(propagation.extract(context.active(), carrier), () => handle(message));
  },
});
```

SQS (`@aws-sdk/client-sqs`): inject into a record, map to `MessageAttributes: { [k]: { DataType: 'String', StringValue: v } }`; receive with `MessageAttributeNames: ['All']` and extract per message. Pub/Sub: `topic.publishMessage({ data, attributes: carrier })` / `propagation.extract(context.active(), message.attributes)`. RabbitMQ (`amqplib`): `channel.publish(ex, key, buf, { headers: carrier })` / `msg.properties.headers`.

BullMQ / Redis Pub/Sub: mirrord filters these on **top-level payload fields**, so put the value there:

```ts
const carrier: Record<string, string> = {};
propagation.inject(context.active(), carrier);
await queue.add(name, { ...data, baggage: carrier.baggage });
// worker
new Worker(name, async (job) =>
  context.with(propagation.extract(context.active(), { baggage: job.data.baggage ?? '' }), () => handle(job)));
```

## Java / Kotlin

Prefer the OTel Java agent (no code) or Spring Boot Micrometer Tracing — see libraries.md. Without either, add `opentelemetry-api` + `opentelemetry-context` and set the propagator once:

Only when the service configures no propagator — otherwise add `W3CBaggagePropagator` to its existing composite or to `otel.propagators`. With the agent, injection happens in every instrumented client; strip `baggage` in an interceptor on clients that call external hosts (OkHttp `Interceptor`, Spring `ClientHttpRequestInterceptor` / `ExchangeFilterFunction` registered after the agent's instrumentation).

```java
TextMapPropagator prop = TextMapPropagator.composite(
    W3CTraceContextPropagator.getInstance(), W3CBaggagePropagator.getInstance());
ContextPropagators propagators = ContextPropagators.create(prop);
```

Kafka with a `ProducerInterceptor` (produce side) and per-record extraction (consume side):

```java
TextMapSetter<Headers> SETTER = (h, k, v) -> { h.remove(k); h.add(k, v.getBytes(UTF_8)); };
TextMapGetter<Headers> GETTER = new TextMapGetter<>() {
  public Iterable<String> keys(Headers h) {
    return StreamSupport.stream(h.spliterator(), false).map(Header::key).toList(); }
  public String get(Headers h, String k) {
    Header x = h.lastHeader(k); return x == null ? null : new String(x.value(), UTF_8); }
};

// ProducerInterceptor.onSend
prop.inject(Context.current(), record.headers(), SETTER);

// consumer loop — per record
for (ConsumerRecord<K, V> r : records) {
  try (Scope s = prop.extract(Context.root(), r.headers(), GETTER).makeCurrent()) { handle(r); }
}
```

Executors: wrap with `Context.taskWrapping(executor)` so tasks inherit the context.

## No dependency allowed

If the team won't take the OTel API, keep the raw header value in the framework's request-scoped context (Go `context.WithValue`, Python `contextvars.ContextVar`, Node `AsyncLocalStorage`, Java `ThreadLocal`/Reactor context) and copy it verbatim onto every outgoing request and message. Don't parse or rewrite it. Note in the report that this layer only carries the raw header and won't interoperate with tracing if tracing is added later.
