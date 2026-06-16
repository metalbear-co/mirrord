"use client";

import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { JsonView, allExpanded, defaultStyles } from "react-json-view-lite";

type Result = { status: number; body: string; elapsedMs: number };
type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

type Endpoint = {
  id: string;
  label: string;
  url: string;
};

type EndpointDialogDraft = {
  mode: "add" | "edit";
  endpointId: string | null;
  label: string;
  url: string;
};

const STORAGE_KEYS = {
  endpoints: "capabilities-rust-frontend-next:endpoints",
  activeEndpointId: "capabilities-rust-frontend-next:active-endpoint-id",
} as const;

const DEFAULT_ENDPOINT: Endpoint = {
  id: "local-backend",
  label: "localhost:8080",
  url: "http://localhost:8080",
};

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
  stringifyStringValues: true,
};

function createEndpointId() {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }

  return `endpoint-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function normalizeEndpointUrl(url: string) {
  return url.trim().replace(/\/+$/, "");
}

function deriveEndpointLabel(url: string) {
  try {
    const parsed = new URL(url);
    const pathname =
      parsed.pathname === "/" ? "" : parsed.pathname.replace(/\/+$/, "");
    return `${parsed.host}${pathname}` || url;
  } catch {
    return url.replace(/^https?:\/\//, "") || "Endpoint";
  }
}

function dedupeEndpointsByUrl(endpoints: Endpoint[]) {
  const seen = new Set<string>();

  return endpoints.filter((endpoint) => {
    const key = normalizeEndpointUrl(endpoint.url).toLowerCase();

    if (seen.has(key)) {
      return false;
    }

    seen.add(key);
    return true;
  });
}

function sanitizeEndpoint(value: unknown): Endpoint | null {
  if (!value || typeof value !== "object") {
    return null;
  }

  const candidate = value as Partial<Endpoint>;

  if (typeof candidate.url !== "string") {
    return null;
  }

  const url = normalizeEndpointUrl(candidate.url);

  if (!url) {
    return null;
  }

  const label =
    typeof candidate.label === "string" && candidate.label.trim().length > 0
      ? candidate.label.trim()
      : deriveEndpointLabel(url);

  return {
    id:
      typeof candidate.id === "string" && candidate.id.trim().length > 0
        ? candidate.id
        : createEndpointId(),
    label,
    url,
  };
}

function loadStoredEndpoints() {
  if (typeof window === "undefined") {
    return null;
  }

  try {
    const rawEndpoints = localStorage.getItem(STORAGE_KEYS.endpoints);

    if (!rawEndpoints) {
      return null;
    }

    const parsed = JSON.parse(rawEndpoints) as unknown;

    if (!Array.isArray(parsed)) {
      return null;
    }

    const endpoints = dedupeEndpointsByUrl(
      parsed
        .map(sanitizeEndpoint)
        .filter((value): value is Endpoint => value !== null),
    );

    if (endpoints.length === 0) {
      return null;
    }

    const storedActiveId = localStorage.getItem(STORAGE_KEYS.activeEndpointId);
    const activeEndpointId = endpoints.some(
      (endpoint) => endpoint.id === storedActiveId,
    )
      ? (storedActiveId as string)
      : endpoints[0].id;

    return {
      endpoints,
      activeEndpointId,
    };
  } catch {
    return null;
  }
}

function saveStoredEndpoints(endpoints: Endpoint[], activeEndpointId: string) {
  if (typeof window === "undefined") {
    return;
  }

  localStorage.setItem(STORAGE_KEYS.endpoints, JSON.stringify(endpoints));
  localStorage.setItem(STORAGE_KEYS.activeEndpointId, activeEndpointId);
}

async function call(
  baseUrl: string,
  path: string,
  init?: RequestInit,
): Promise<Result> {
  const start = performance.now();
  const response = await fetch(`${baseUrl}${path}`, init);
  const body = await response.text();
  return {
    status: response.status,
    body,
    elapsedMs: Math.round((performance.now() - start) * 100) / 100,
  };
}

function Pane({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: ReactNode;
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
    return {
      metaLine: "No response yet.",
      rawBody: "",
      parsedJson: null as JsonValue | null,
    };
  }

  if (result.startsWith("error:")) {
    return {
      metaLine: "Request failed",
      rawBody: result,
      parsedJson: null as JsonValue | null,
    };
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

function EndpointSwitcher({
  endpoints,
  activeEndpointId,
  onAdd,
  onEdit,
  onSelect,
}: {
  endpoints: Endpoint[];
  activeEndpointId: string;
  onAdd: () => void;
  onEdit: (endpoint: Endpoint) => void;
  onSelect: (endpointId: string) => void;
}) {
  const activeButtonRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    activeButtonRef.current?.scrollIntoView({
      block: "nearest",
      inline: "center",
      behavior: "smooth",
    });
  }, [activeEndpointId, endpoints]);

  return (
    <div className="endpoint-scroll-shell">
      <button
        className="endpoint-add"
        type="button"
        onClick={onAdd}
        aria-label="Add endpoint"
        title="Add endpoint"
      >
        +
      </button>

      <div className="endpoint-scroll" aria-label="Saved endpoints">
        <div className="endpoint-rail">
          {endpoints.map((endpoint) => {
            const isActive = endpoint.id === activeEndpointId;

            return (
              <div
                key={endpoint.id}
                className={`endpoint-pill-group${isActive ? " is-active" : ""}`}
              >
                <button
                  ref={isActive ? activeButtonRef : undefined}
                  type="button"
                  className={`endpoint-pill${isActive ? " is-active" : ""}`}
                  onClick={() => onSelect(endpoint.id)}
                  aria-pressed={isActive}
                  title={endpoint.url}
                >
                  <span className="endpoint-pill-label">{endpoint.label}</span>
                </button>
                <button
                  type="button"
                  className="endpoint-edit"
                  onClick={() => onEdit(endpoint)}
                  aria-label={`Edit ${endpoint.label}`}
                  title="Edit endpoint"
                >
                  ✎
                </button>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function EndpointDialog({
  draft,
  onCancel,
  onSave,
}: {
  draft: EndpointDialogDraft;
  onCancel: () => void;
  onSave: (draft: EndpointDialogDraft) => void;
}) {
  const [label, setLabel] = useState(draft.label);
  const [url, setUrl] = useState(draft.url);
  const [error, setError] = useState<string>("");
  const urlRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    urlRef.current?.focus();
  }, []);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        onCancel();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onCancel]);

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();

    const normalizedUrl = normalizeEndpointUrl(url);

    if (!normalizedUrl) {
      setError("Endpoint URL is required.");
      return;
    }

    onSave({
      ...draft,
      label,
      url: normalizedUrl,
    });
  }

  return (
    <section
      className="endpoint-dialog"
      role="dialog"
      aria-modal="false"
      aria-label="Endpoint editor"
    >
      <div className="endpoint-dialog-header">
        <div>
          <h4>{draft.mode === "add" ? "Add endpoint" : "Edit endpoint"}</h4>
          <p>
            {draft.mode === "add"
              ? "Save a new target once and switch to it later."
              : "Update the selected saved target."}
          </p>
        </div>
      </div>

      <form className="endpoint-dialog-form" onSubmit={handleSubmit}>
        <label className="endpoint-field">
          <span>Label</span>
          <input
            className="input"
            value={label}
            onChange={(event) => {
              setError("");
              setLabel(event.target.value);
            }}
            placeholder="staging"
          />
        </label>

        <label className="endpoint-field">
          <span>Endpoint URL</span>
          <input
            ref={urlRef}
            className="input"
            value={url}
            onChange={(event) => {
              setError("");
              setUrl(event.target.value);
            }}
            placeholder="http://localhost:8080"
          />
        </label>

        {error ? <p className="endpoint-dialog-error">{error}</p> : null}

        <div className="endpoint-dialog-actions">
          <button
            type="button"
            className="endpoint-dialog-cancel"
            onClick={onCancel}
          >
            Cancel
          </button>
          <button type="submit">
            {draft.mode === "add" ? "Add endpoint" : "Save changes"}
          </button>
        </div>
      </form>
    </section>
  );
}

export default function Home() {
  const [endpoints, setEndpoints] = useState<Endpoint[]>([DEFAULT_ENDPOINT]);
  const [activeEndpointId, setActiveEndpointId] = useState(DEFAULT_ENDPOINT.id);
  const [endpointDraft, setEndpointDraft] =
    useState<EndpointDialogDraft | null>(null);
  const [storageReady, setStorageReady] = useState(false);

  const [dnsHost, setDnsHost] = useState<string>("example.com");
  const [outgoingUrl, setOutgoingUrl] = useState<string>("https://example.com");
  const [echoBody, setEchoBody] = useState<string>("hello from next frontend");

  const [health, setHealth] = useState<string>("");
  const [meta, setMeta] = useState<string>("");
  const [env, setEnv] = useState<string>("");
  const [dns, setDns] = useState<string>("");
  const [outgoing, setOutgoing] = useState<string>("");
  const [echo, setEcho] = useState<string>("");

  const activeEndpoint =
    endpoints.find((endpoint) => endpoint.id === activeEndpointId) ??
    endpoints[0];
  const normalizedBase = useMemo(
    () => normalizeEndpointUrl(activeEndpoint.url),
    [activeEndpoint.url],
  );
  const toolbarRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const stored = loadStoredEndpoints();

    if (stored !== null) {
      setEndpoints(stored.endpoints);
      setActiveEndpointId(stored.activeEndpointId);
    }

    setStorageReady(true);
  }, []);

  useEffect(() => {
    if (!storageReady) {
      return;
    }

    saveStoredEndpoints(endpoints, activeEndpointId);
  }, [endpoints, activeEndpointId, storageReady]);

  useEffect(() => {
    if (!endpointDraft) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      if (toolbarRef.current === null) {
        return;
      }

      if (
        event.target instanceof Node &&
        !toolbarRef.current.contains(event.target)
      ) {
        setEndpointDraft(null);
      }
    }

    window.addEventListener("pointerdown", handlePointerDown);
    return () => window.removeEventListener("pointerdown", handlePointerDown);
  }, [endpointDraft]);

  const run = async (
    setter: (value: string) => void,
    req: () => Promise<Result>,
  ) => {
    try {
      const result = await req();
      setter(
        `status: ${result.status} (${result.elapsedMs}ms)\n\n${result.body}`,
      );
    } catch (error) {
      setter(`error: ${String(error)}`);
    }
  };

  function openAddDialog() {
    setEndpointDraft({
      mode: "add",
      endpointId: null,
      label: "",
      url: "",
    });
  }

  function openEditDialog(endpoint: Endpoint) {
    setEndpointDraft({
      mode: "edit",
      endpointId: endpoint.id,
      label: endpoint.label,
      url: endpoint.url,
    });
  }

  function saveDraft(draft: EndpointDialogDraft) {
    const normalizedUrl = normalizeEndpointUrl(draft.url);

    if (!normalizedUrl) {
      return;
    }

    const label =
      draft.label.trim().length > 0
        ? draft.label.trim()
        : deriveEndpointLabel(normalizedUrl);
    const normalizedTarget = normalizedUrl.toLowerCase();
    const duplicateIndex = endpoints.findIndex(
      (endpoint) =>
        normalizeEndpointUrl(endpoint.url).toLowerCase() === normalizedTarget,
    );

    if (draft.mode === "add") {
      if (duplicateIndex >= 0) {
        setActiveEndpointId(endpoints[duplicateIndex].id);
        setEndpointDraft(null);
        return;
      }

      const created = {
        id: createEndpointId(),
        label,
        url: normalizedUrl,
      };

      setEndpoints([...endpoints, created]);
      setActiveEndpointId(created.id);
      setEndpointDraft(null);
      return;
    }

    if (draft.endpointId === null) {
      const created = {
        id: createEndpointId(),
        label,
        url: normalizedUrl,
      };

      setEndpoints([...endpoints, created]);
      setActiveEndpointId(created.id);
      setEndpointDraft(null);
      return;
    }

    const editingIndex = endpoints.findIndex(
      (endpoint) => endpoint.id === draft.endpointId,
    );

    if (editingIndex < 0) {
      setEndpointDraft(null);
      return;
    }

    if (
      duplicateIndex >= 0 &&
      endpoints[duplicateIndex].id !== draft.endpointId
    ) {
      const merged = endpoints
        .map((endpoint) => {
          if (endpoint.id === endpoints[duplicateIndex].id) {
            return {
              ...endpoint,
              label,
              url: normalizedUrl,
            };
          }

          return endpoint;
        })
        .filter((endpoint) => endpoint.id !== draft.endpointId);

      setEndpoints(merged);
      setActiveEndpointId(endpoints[duplicateIndex].id);
      setEndpointDraft(null);
      return;
    }

    const nextEndpoints = endpoints.map((endpoint) => {
      if (endpoint.id !== draft.endpointId) {
        return endpoint;
      }

      return {
        ...endpoint,
        label,
        url: normalizedUrl,
      };
    });

    setEndpoints(nextEndpoints);
    setActiveEndpointId(draft.endpointId);
    setEndpointDraft(null);
  }

  return (
    <main>
      <section className="hero">
        <div className="hero-inner">
          <span className="eyebrow">Capabilities demo</span>
          <h1>
            Test live endpoints with a <em>friendlier API workbench</em>.
          </h1>
          <p>
            Point this UI at your backend, run quick checks, and inspect each
            response as readable JSON when available.
          </p>
        </div>
      </section>
      <svg
        className="wave-divider"
        viewBox="0 0 1440 64"
        preserveAspectRatio="none"
      >
        <path
          d="M0,32 C160,64 320,0 480,32 C640,64 800,0 960,32 C1120,64 1280,0 1440,32 L1440,64 L0,64 Z"
          fill="#fafafd"
        />
      </svg>

      <section className="main-wrap">
        <div className="endpoint-bar" ref={toolbarRef}>
          <EndpointSwitcher
            endpoints={endpoints}
            activeEndpointId={activeEndpointId}
            onAdd={openAddDialog}
            onEdit={openEditDialog}
            onSelect={setActiveEndpointId}
          />

          {endpointDraft !== null ? (
            <EndpointDialog
              key={`${endpointDraft.mode}:${endpointDraft.endpointId ?? "new"}:${endpointDraft.url}`}
              draft={endpointDraft}
              onCancel={() => setEndpointDraft(null)}
              onSave={saveDraft}
            />
          ) : null}
        </div>

        <div className="grid">
          <Pane
            title="Health"
            subtitle="Verify the service is reachable and healthy."
          >
            <div className="row">
              <input className="input" value="/healthz" readOnly />
              <button
                onClick={() =>
                  run(setHealth, () => call(normalizedBase, "/healthz"))
                }
              >
                GET
              </button>
            </div>
            <div className="result-block">
              <ResultView result={health} />
            </div>
          </Pane>

          <Pane
            title="Meta"
            subtitle="Inspect runtime metadata from the backend process."
          >
            <div className="row">
              <input className="input" value="/meta" readOnly />
              <button
                onClick={() =>
                  run(setMeta, () => call(normalizedBase, "/meta"))
                }
              >
                GET
              </button>
            </div>
            <div className="result-block">
              <ResultView result={meta} />
            </div>
          </Pane>

          <Pane
            title="Env"
            subtitle="List filtered environment details exposed by the API."
          >
            <div className="row">
              <input className="input" value="/env" readOnly />
              <button
                onClick={() => run(setEnv, () => call(normalizedBase, "/env"))}
              >
                GET
              </button>
            </div>
            <div className="result-block">
              <ResultView result={env} />
            </div>
          </Pane>

          <Pane
            title="DNS"
            subtitle="Resolve a host from the backend network namespace."
          >
            <div className="row">
              <input
                className="input"
                value={dnsHost}
                onChange={(event) => setDnsHost(event.target.value)}
              />
              <button
                onClick={() =>
                  run(setDns, () =>
                    call(
                      normalizedBase,
                      `/dns?host=${encodeURIComponent(dnsHost)}`,
                    ),
                  )
                }
              >
                GET
              </button>
            </div>
            <div className="result-block">
              <ResultView result={dns} />
            </div>
          </Pane>

          <Pane
            title="Outgoing HTTP"
            subtitle="Run an outbound fetch from backend to target URL."
          >
            <div className="row">
              <input
                className="input"
                value={outgoingUrl}
                onChange={(event) => setOutgoingUrl(event.target.value)}
              />
              <button
                onClick={() =>
                  run(setOutgoing, () =>
                    call(
                      normalizedBase,
                      `/outgoing?url=${encodeURIComponent(outgoingUrl)}`,
                    ),
                  )
                }
              >
                GET
              </button>
            </div>
            <div className="result-block">
              <ResultView result={outgoing} />
            </div>
          </Pane>

          <Pane
            title="Echo"
            subtitle="Send a payload and confirm body roundtrip behavior."
          >
            <div className="row">
              <input className="input" value="/echo" readOnly />
              <button
                onClick={() =>
                  run(setEcho, () =>
                    call(normalizedBase, "/echo", {
                      method: "POST",
                      body: echoBody,
                    }),
                  )
                }
              >
                POST
              </button>
            </div>
            <textarea
              value={echoBody}
              onChange={(event) => setEchoBody(event.target.value)}
            />
            <div className="result-block">
              <ResultView result={echo} />
            </div>
          </Pane>
        </div>
      </section>
    </main>
  );
}
