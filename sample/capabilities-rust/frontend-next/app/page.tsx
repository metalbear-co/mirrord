"use client";

import { useMemo, useState } from "react";
import { JsonView, allExpanded, defaultStyles } from "react-json-view-lite";

type Result = { status: number; body: string; elapsedMs: number };
type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
const jsonViewStyles = {
  ...defaultStyles,
  container: "mb-json-container",
  basicChildStyle: "mb-json-child",
  label: "mb-json-label",
  clickableLabel: "mb-json-clickable-label",
  nullValue: "mb-json-null",
  undefinedValue: "mb-json-undefined",
  numberValue: "mb-json-number",
  stringValue: "mb-json-string",
  booleanValue: "mb-json-boolean",
  otherValue: "mb-json-other",
  punctuation: "mb-json-punctuation",
  expandIcon: "mb-json-expand",
  collapseIcon: "mb-json-collapse",
  collapsedContent: "mb-json-collapsed",
  childFieldsContainer: "mb-json-children",
  quotesForFieldNames: true,
  stringifyStringValues: true
};

async function call(baseUrl: string, path: string, init?: RequestInit): Promise<Result> {
  const start = performance.now();
  const response = await fetch(`${baseUrl}${path}`, init);
  const body = await response.text();
  return {
    status: response.status,
    body,
    elapsedMs: Math.round((performance.now() - start) * 100) / 100
  };
}

function Pane({
  title,
  subtitle,
  children
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <section className="pane">
      <h3>{title}</h3>
      <p>{subtitle}</p>
      {children}
    </section>
  );
}

function sortJsonKeysRecursively(value: JsonValue): JsonValue {
  if (Array.isArray(value)) {
    return value.map(sortJsonKeysRecursively);
  }

  if (value && typeof value === "object") {
    return Object.keys(value)
      .sort((left, right) => left.localeCompare(right))
      .reduce<{ [key: string]: JsonValue }>((accumulator, key) => {
        accumulator[key] = sortJsonKeysRecursively(value[key]);
        return accumulator;
      }, {});
  }

  return value;
}

function parseResult(result: string) {
  if (!result) {
    return { metaLine: "No response yet.", rawBody: "", parsedJson: null as JsonValue | null };
  }

  if (result.startsWith("error:")) {
    return { metaLine: "Request failed", rawBody: result, parsedJson: null as JsonValue | null };
  }

  const [metaLine, ...rest] = result.split("\n");
  const rawBody = rest.join("\n").trimStart();

  try {
    const parsed = JSON.parse(rawBody) as JsonValue;
    return { metaLine, rawBody, parsedJson: sortJsonKeysRecursively(parsed) };
  } catch {
    return { metaLine, rawBody, parsedJson: null as JsonValue | null };
  }
}

function ResultView({ result }: { result: string }) {
  const parsed = parseResult(result);

  return (
    <>
      <span className="meta">{parsed.metaLine}</span>
      {parsed.parsedJson !== null ? (
        <div className="json-viewer">
          <JsonView
            data={parsed.parsedJson}
            shouldExpandNode={allExpanded}
            style={jsonViewStyles}
            clickToExpandNode
          />
        </div>
      ) : (
        parsed.rawBody || "No response yet."
      )}
    </>
  );
}

export default function Home() {
  const [baseUrl, setBaseUrl] = useState("http://localhost:8080");
  const [dnsHost, setDnsHost] = useState("example.com");
  const [outgoingUrl, setOutgoingUrl] = useState("https://example.com");
  const [echoBody, setEchoBody] = useState("hello from next frontend");

  const [health, setHealth] = useState<string>("");
  const [meta, setMeta] = useState<string>("");
  const [env, setEnv] = useState<string>("");
  const [dns, setDns] = useState<string>("");
  const [outgoing, setOutgoing] = useState<string>("");
  const [echo, setEcho] = useState<string>("");

  const normalizedBase = useMemo(() => baseUrl.replace(/\/$/, ""), [baseUrl]);

  const run = async (setter: (value: string) => void, req: () => Promise<Result>) => {
    try {
      const result = await req();
      setter(`status: ${result.status} (${result.elapsedMs}ms)\n\n${result.body}`);
    } catch (error) {
      setter(`error: ${String(error)}`);
    }
  };

  return (
    <main>
      <section className="hero">
        <div className="hero-inner">
          <span className="eyebrow">Capabilities demo</span>
          <h1>
            Test live endpoints with a <em>friendlier API workbench</em>.
          </h1>
          <p>
            Point this UI at your backend, run quick checks, and inspect each response as readable
            JSON when available.
          </p>
        </div>
      </section>
      <svg className="wave-divider" viewBox="0 0 1440 64" preserveAspectRatio="none">
        <path
          d="M0,32 C160,64 320,0 480,32 C640,64 800,0 960,32 C1120,64 1280,0 1440,32 L1440,64 L0,64 Z"
          fill="#fafafd"
        />
      </svg>

      <section className="main-wrap">
        <div className="topbar">
        <input
          className="input"
          value={baseUrl}
          onChange={(event) => setBaseUrl(event.target.value)}
        />
        <button onClick={() => setBaseUrl("http://localhost:8080")}>Use Local Backend</button>
      </div>

      <div className="grid">
        <Pane title="Health" subtitle="Verify the service is reachable and healthy.">
          <div className="row">
            <input className="input" value="/healthz" readOnly />
            <button onClick={() => run(setHealth, () => call(normalizedBase, "/healthz"))}>
              GET
            </button>
          </div>
          <div className="result-block"><ResultView result={health} /></div>
        </Pane>

        <Pane title="Meta" subtitle="Inspect runtime metadata from the backend process.">
          <div className="row">
            <input className="input" value="/meta" readOnly />
            <button onClick={() => run(setMeta, () => call(normalizedBase, "/meta"))}>GET</button>
          </div>
          <div className="result-block"><ResultView result={meta} /></div>
        </Pane>

        <Pane title="Env" subtitle="List filtered environment details exposed by the API.">
          <div className="row">
            <input className="input" value="/env" readOnly />
            <button onClick={() => run(setEnv, () => call(normalizedBase, "/env"))}>GET</button>
          </div>
          <div className="result-block"><ResultView result={env} /></div>
        </Pane>

        <Pane title="DNS" subtitle="Resolve a host from the backend network namespace.">
          <div className="row">
            <input
              className="input"
              value={dnsHost}
              onChange={(event) => setDnsHost(event.target.value)}
            />
            <button
              onClick={() =>
                run(setDns, () => call(normalizedBase, `/dns?host=${encodeURIComponent(dnsHost)}`))
              }
            >
              GET
            </button>
          </div>
          <div className="result-block"><ResultView result={dns} /></div>
        </Pane>

        <Pane title="Outgoing HTTP" subtitle="Run an outbound fetch from backend to target URL.">
          <div className="row">
            <input
              className="input"
              value={outgoingUrl}
              onChange={(event) => setOutgoingUrl(event.target.value)}
            />
            <button
              onClick={() =>
                run(
                  setOutgoing,
                  () => call(normalizedBase, `/outgoing?url=${encodeURIComponent(outgoingUrl)}`)
                )
              }
            >
              GET
            </button>
          </div>
          <div className="result-block"><ResultView result={outgoing} /></div>
        </Pane>

        <Pane title="Echo" subtitle="Send a payload and confirm body roundtrip behavior.">
          <div className="row">
            <input className="input" value="/echo" readOnly />
            <button
              onClick={() =>
                run(setEcho, () =>
                  call(normalizedBase, "/echo", {
                    method: "POST",
                    body: echoBody
                  })
                )
              }
            >
              POST
            </button>
          </div>
          <textarea value={echoBody} onChange={(event) => setEchoBody(event.target.value)} />
          <div className="result-block"><ResultView result={echo} /></div>
        </Pane>
      </div>
      </section>
    </main>
  );
}
