"use client";

import type { FormEvent } from "react";
import { JsonView, allExpanded, defaultStyles } from "react-json-view-lite";

export type Result = { status: number; body: string; elapsedMs: number };
export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD";

type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

const HTTP_METHODS: HttpMethod[] = [
  "GET",
  "POST",
  "PUT",
  "PATCH",
  "DELETE",
  "HEAD",
];

const CLASSIFIED_KEYS = new Set([
  "token",
  "secretaccesskey",
  "mirrord_remote_apikey",
]);

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

function redactClassifiedValues(value: JsonValue): JsonValue {
  if (Array.isArray(value)) {
    return value.map(redactClassifiedValues);
  }

  if (value && typeof value === "object") {
    return Object.keys(value).reduce<{ [key: string]: JsonValue }>(
      (accumulator, key) => {
        accumulator[key] = CLASSIFIED_KEYS.has(key.toLowerCase())
          ? "REDACTED"
          : redactClassifiedValues(value[key]);
        return accumulator;
      },
      {},
    );
  }

  return value;
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

function prepareJsonForDisplay(value: JsonValue): JsonValue {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return redactClassifiedValues(value);
  }

  const result = { ...value };
  const body = result.body;

  if (typeof body === "string") {
    try {
      result.body = JSON.parse(body) as JsonValue;
      delete result.body_preview;
    } catch {
      delete result.body;
    }
  }

  return redactClassifiedValues(result);
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
    return {
      metaLine,
      rawBody,
      parsedJson: sortJsonKeysRecursively(prepareJsonForDisplay(parsed)),
    };
  } catch {
    return { metaLine, rawBody, parsedJson: null as JsonValue | null };
  }
}

export function ResultView({ result }: { result: string }) {
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

type RequestTabProps = {
  path: string;
  result: string;
  onRun: () => void;
};

function RequestTab({ path, result, onRun }: RequestTabProps) {
  return (
    <>
      <div className="row">
        <input className="input" value={path} readOnly />
        <button onClick={onRun}>GET</button>
      </div>
      <div className="result-block">
        <ResultView result={result} />
      </div>
    </>
  );
}

export function MetaTab({ result, onRun }: Omit<RequestTabProps, "path">) {
  return <RequestTab path="/meta" result={result} onRun={onRun} />;
}

export function EnvTab({ result, onRun }: Omit<RequestTabProps, "path">) {
  return <RequestTab path="/env" result={result} onRun={onRun} />;
}

type OutgoingTabProps = {
  result: string;
  url: string;
  onUrlChange: (url: string) => void;
  onRun: () => void;
};

export function OutgoingTab({
  result,
  url,
  onUrlChange,
  onRun,
}: OutgoingTabProps) {
  return (
    <>
      <div className="row">
        <input
          className="input"
          value={url}
          onChange={(event) => onUrlChange(event.target.value)}
        />
        <button onClick={onRun}>GET</button>
      </div>
      <div className="result-block">
        <ResultView result={result} />
      </div>
    </>
  );
}

type CustomTabProps = {
  result: string;
  method: HttpMethod;
  path: string;
  headers: string;
  body: string;
  onMethodChange: (method: HttpMethod) => void;
  onPathChange: (path: string) => void;
  onHeadersChange: (headers: string) => void;
  onBodyChange: (body: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
};

export function CustomTab({
  result,
  method,
  path,
  headers,
  body,
  onMethodChange,
  onPathChange,
  onHeadersChange,
  onBodyChange,
  onSubmit,
}: CustomTabProps) {
  return (
    <form className="custom-request" onSubmit={onSubmit}>
      <div className="custom-request-target">
        <select
          className="input custom-request-method"
          value={method}
          onChange={(event) => onMethodChange(event.target.value as HttpMethod)}
          aria-label="HTTP method"
        >
          {HTTP_METHODS.map((availableMethod) => (
            <option key={availableMethod} value={availableMethod}>
              {availableMethod}
            </option>
          ))}
        </select>
        <input
          className="input"
          value={path}
          onChange={(event) => onPathChange(event.target.value)}
          placeholder="/endpoint?query=value"
          aria-label="Endpoint path"
          required
        />
        <button type="submit">Send</button>
      </div>

      <div className="custom-request-options">
        <label className="custom-request-field">
          <span>Headers (JSON)</span>
          <textarea
            value={headers}
            onChange={(event) => onHeadersChange(event.target.value)}
            placeholder={'{"Content-Type": "application/json"}'}
          />
        </label>
        <label className="custom-request-field">
          <span>Request body</span>
          <textarea
            value={body}
            onChange={(event) => onBodyChange(event.target.value)}
            placeholder='{"example": true}'
            disabled={method === "GET" || method === "HEAD"}
          />
        </label>
      </div>

      <div className="result-block">
        <ResultView result={result} />
      </div>
    </form>
  );
}
