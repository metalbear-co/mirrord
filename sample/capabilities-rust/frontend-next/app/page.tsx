"use client";

import {
  type FormEvent,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import defaultEndpoints from "./default-endpoints.json";
import {
  CustomTab,
  EnvTab,
  MetaTab,
  OutgoingTab,
  type HttpMethod,
  type Result,
} from "./workbench-tabs";

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

type WorkbenchTabId = "meta" | "env" | "outgoing" | "custom";

type WorkbenchTab = {
  id: WorkbenchTabId;
  label: string;
  title: string;
  subtitle: string;
};

const WORKBENCH_TABS: WorkbenchTab[] = [
  {
    id: "meta",
    label: "Meta",
    title: "Read backend metadata",
    subtitle: "Request metadata from the backend and view its response."
  },
  {
    id: "env",
    label: "Env",
    title: "Read backend environment",
    subtitle: "Request filtered environment details and view the backend response."
  },
  {
    id: "outgoing",
    label: "Outgoing HTTP",
    title: "Fetch a URL through the backend",
    subtitle: "The backend makes the outbound request; this UI shows its response."
  },
  {
    id: "custom",
    label: "Advanced",
    title: "Call a backend endpoint",
    subtitle: "Send a request to the backend and view its response."
  },
] as const;

const STORAGE_KEYS = {
  endpoints: "capabilities-rust-frontend-next:endpoints",
  activeEndpointId: "capabilities-rust-frontend-next:active-endpoint-id",
} as const;

const DEFAULT_ENDPOINTS = defaultEndpoints as Endpoint[];

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

function WorkbenchTabButton({
  tab,
  active,
  onClick,
}: {
  tab: WorkbenchTab;
  active: boolean;
  onClick: (tabId: WorkbenchTabId) => void;
}) {
  return (
    <button
      id={`workbench-tab-${tab.id}`}
      type="button"
      className={`workbench-tab${active ? " is-active" : ""}`}
      role="tab"
      aria-selected={active}
      aria-controls="workbench-panel"
      onClick={() => onClick(tab.id)}
    >
      {tab.label}
    </button>
  );
}



function EndpointSwitcher({
  endpoints,
  activeEndpointId,
  onAdd,
  onEdit,
  onReset,
  onSelect,
}: {
  endpoints: Endpoint[];
  activeEndpointId: string;
  onAdd: () => void;
  onEdit: (endpoint: Endpoint) => void;
  onReset: () => void;
  onSelect: (endpointId: string) => void;
}) {
  const activeEndpoint =
    endpoints.find((endpoint) => endpoint.id === activeEndpointId) ?? endpoints[0];

  return (
    <div className="endpoint-switcher">
      <div className="endpoint-switcher-header">
        <label htmlFor="backend-select">Select backend</label>
        <div className="endpoint-switcher-actions">
          <button
            className="endpoint-action endpoint-add"
            type="button"
            onClick={onAdd}
            aria-label="Add backend"
            title="Add backend"
          >
            +
          </button>
          <button
            className="endpoint-action endpoint-reset"
            type="button"
            onClick={onReset}
            aria-label="Reset backends to defaults"
            title="Reset to default backends"
          >
            ↺
          </button>
        </div>
      </div>
      <div className="endpoint-switcher-selection">
        <select
          id="backend-select"
          value={activeEndpointId}
          onChange={(event) => onSelect(event.target.value)}
          title={activeEndpoint.url}
        >
          {endpoints.map((endpoint) => (
            <option key={endpoint.id} value={endpoint.id}>
              {endpoint.label}
            </option>
          ))}
        </select>
        <button
          className="endpoint-action endpoint-edit"
          type="button"
          onClick={() => onEdit(activeEndpoint)}
          aria-label={`Edit ${activeEndpoint.label}`}
          title="Edit selected backend"
        >
          ✎
        </button>
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
  const [endpoints, setEndpoints] = useState<Endpoint[]>(DEFAULT_ENDPOINTS);
  const [activeEndpointId, setActiveEndpointId] = useState(
    DEFAULT_ENDPOINTS[0].id,
  );
  const [endpointDraft, setEndpointDraft] =
    useState<EndpointDialogDraft | null>(null);
  const [storageReady, setStorageReady] = useState(false);
  const [activeTab, setActiveTab] = useState<WorkbenchTabId>("meta");

  const [outgoingUrl, setOutgoingUrl] = useState<string>(
    "http://169.254.170.2/v2/metadata",
  );
  const [customMethod, setCustomMethod] = useState<HttpMethod>("GET");
  const [customPath, setCustomPath] = useState<string>("/");
  const [customHeaders, setCustomHeaders] = useState<string>("");
  const [customBody, setCustomBody] = useState<string>("");

  const [meta, setMeta] = useState<string>("");
  const [env, setEnv] = useState<string>("");
  const [outgoing, setOutgoing] = useState<string>("");
  const [custom, setCustom] = useState<string>("");

  const activeEndpoint =
    endpoints.find((endpoint) => endpoint.id === activeEndpointId) ??
    endpoints[0];
  const normalizedBase = useMemo(
    () => normalizeEndpointUrl(activeEndpoint.url),
    [activeEndpoint.url],
  );
  const toolbarRef = useRef<HTMLDivElement | null>(null);
  const activeWorkbenchTab =
    WORKBENCH_TABS.find((tab) => tab.id === activeTab) ?? WORKBENCH_TABS[0];

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

  function runCustomEndpoint(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();

    let headers: HeadersInit | undefined;

    if (customHeaders.trim()) {
      try {
        const parsed = JSON.parse(customHeaders) as unknown;

        if (parsed === null || Array.isArray(parsed) || typeof parsed !== "object") {
          throw new Error("headers must be a JSON object");
        }

        if (
          Object.values(parsed).some(
            (value) => typeof value !== "string",
          )
        ) {
          throw new Error("header values must be strings");
        }

        headers = parsed as Record<string, string>;
      } catch (error) {
        setCustom(`error: Invalid headers: ${String(error)}`);
        return;
      }
    }

    const path = customPath.trim();
    const normalizedPath = path.startsWith("/") ? path : `/${path}`;
    const supportsBody = customMethod !== "GET" && customMethod !== "HEAD";

    run(setCustom, () =>
      call(normalizedBase, normalizedPath, {
        method: customMethod,
        headers,
        body: supportsBody && customBody ? customBody : undefined,
      }),
    );
  }

  function openAddDialog() {
    setEndpointDraft({
      mode: "add",
      endpointId: null,
      label: "",
      url: "",
    });
  }

  function resetEndpoints() {
    if (typeof window !== "undefined") {
      localStorage.removeItem(STORAGE_KEYS.endpoints);
      localStorage.removeItem(STORAGE_KEYS.activeEndpointId);
    }

    setEndpointDraft(null);
    setEndpoints(DEFAULT_ENDPOINTS);
    setActiveEndpointId(DEFAULT_ENDPOINTS[0].id);
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
          <div className="hero-copy">
            <span className="eyebrow">Capabilities demo</span>
            <h1>
              Test live endpoints with a <em>friendlier API workbench</em>.
            </h1>
            <p>
              Point this UI at your backend, run quick checks, and inspect each
              response as readable JSON when available.
            </p>
          </div>

          <div className="endpoint-control" ref={toolbarRef}>
            <EndpointSwitcher
              endpoints={endpoints}
              activeEndpointId={activeEndpointId}
              onAdd={openAddDialog}
              onEdit={openEditDialog}
              onReset={resetEndpoints}
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
        </div>
      </section>

      <section className="main-wrap">
        <section className="pane workbench-pane" aria-label="Backend workbench">
          <div className="workbench-tabs-shell">
            <div className="workbench-tabs" role="tablist" aria-label="Backend tools">
              {WORKBENCH_TABS.map((tab) => (
                <WorkbenchTabButton
                  key={tab.id}
                  tab={tab}
                  active={tab.id === activeTab}
                  onClick={setActiveTab}
                />
              ))}
            </div>
          </div>

          <div className="workbench-panel" id="workbench-panel" role="tabpanel" aria-labelledby={`workbench-tab-${activeTab}`}>
            <h3>{activeWorkbenchTab.title}</h3>
            <p>{activeWorkbenchTab.subtitle}</p>

            {activeTab === "meta" ? (
              <MetaTab
                result={meta}
                onRun={() => run(setMeta, () => call(normalizedBase, "/meta"))}
              />
            ) : null}

            {activeTab === "env" ? (
              <EnvTab
                result={env}
                onRun={() => run(setEnv, () => call(normalizedBase, "/env"))}
              />
            ) : null}

            {activeTab === "outgoing" ? (
              <OutgoingTab
                result={outgoing}
                url={outgoingUrl}
                onUrlChange={setOutgoingUrl}
                onRun={() =>
                  run(setOutgoing, () =>
                    call(
                      normalizedBase,
                      `/outgoing?url=${encodeURIComponent(outgoingUrl)}`,
                    ),
                  )
                }
              />
            ) : null}

            {activeTab === "custom" ? (
              <CustomTab
                result={custom}
                method={customMethod}
                path={customPath}
                headers={customHeaders}
                body={customBody}
                onMethodChange={setCustomMethod}
                onPathChange={setCustomPath}
                onHeadersChange={setCustomHeaders}
                onBodyChange={setCustomBody}
                onSubmit={runCustomEndpoint}
              />
            ) : null}
          </div>
        </section>
      </section>
    </main>
  );
}
