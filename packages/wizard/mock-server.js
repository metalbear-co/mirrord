import http from "http";

// Stands in for the `mirrord ui` server routes the wizard calls, for working on the wizard without
// a cluster. The modes are the ones `GET /api/v1/up/modes` serves.
const mockData = {
  modes: [
    {
      id: "split",
      label: "Split",
      description:
        "Your requests reach your local process. Everyone else's keep hitting the deployed service, so you can work in a shared cluster without disrupting anyone.",
      recommended: true,
      target_types: ["deployment", "statefulset", "rollout", "pod"],
      targetless: true,
      http_filter: true,
      traffic_question: "Configure traffic filtering and ports?",
    },
    {
      id: "mirror",
      label: "Mirror",
      description:
        "A copy of your session's traffic is sent to your local process. The deployed service still handles every request, including yours.",
      recommended: false,
      target_types: ["deployment", "statefulset", "rollout", "pod"],
      targetless: false,
      http_filter: true,
      traffic_question: "Configure traffic filtering and ports?",
    },
    {
      id: "replace",
      label: "Replace",
      description:
        "Every request reaches your local process. The deployed workload is scaled to zero for the session and restored when it ends.",
      recommended: false,
      target_types: ["deployment", "statefulset"],
      targetless: false,
      http_filter: false,
      traffic_question: "Configure ports?",
    },
  ],
  contexts: {
    contexts: [{ name: "mock-cluster", namespace: null }],
    current: "mock-cluster",
  },
  namespaces: ["default", "production", "staging", "development"],
  targets: {
    default: [
      {
        target_path: "deployment/api-server",
        target_namespace: "default",
        containers: ["api", "sidecar"],
        detected_ports: [8080, 3000],
      },
      {
        target_path: "deployment/web-frontend",
        target_namespace: "default",
        containers: ["web"],
        detected_ports: [80, 443],
      },
      {
        target_path: "pod/redis-master-0",
        target_namespace: "default",
        containers: ["redis"],
        detected_ports: [6379],
      },
      {
        target_path: "statefulset/postgres",
        target_namespace: "default",
        containers: ["postgres"],
        detected_ports: [5432],
      },
    ],
    production: [
      {
        target_path: "deployment/prod-api",
        target_namespace: "production",
        containers: ["api", "metrics"],
        detected_ports: [8080],
      },
      {
        target_path: "deployment/prod-worker",
        target_namespace: "production",
        containers: ["worker"],
        detected_ports: [9090],
      },
      {
        target_path: "pod/prod-api-abc123",
        target_namespace: "production",
        containers: ["api", "sidecar"],
        detected_ports: [8080],
      },
      {
        target_path: "pod/prod-worker-xyz789",
        target_namespace: "production",
        containers: ["worker"],
        detected_ports: [9090],
      },
      {
        target_path: "rollout/prod-canary",
        target_namespace: "production",
        containers: ["api"],
        detected_ports: [8080],
      },
    ],
    staging: [
      {
        target_path: "deployment/staging-api",
        target_namespace: "staging",
        containers: ["api", "debug"],
        detected_ports: [8080, 9090],
      },
      {
        target_path: "deployment/staging-web",
        target_namespace: "staging",
        containers: ["web"],
        detected_ports: [3000],
      },
    ],
    development: [
      {
        target_path: "deployment/dev-api",
        target_namespace: "development",
        containers: ["api"],
        detected_ports: [8080],
      },
      {
        target_path: "pod/dev-debug",
        target_namespace: "development",
        containers: ["debug"],
        detected_ports: [5005],
      },
    ],
  },
};

// A rough YAML for the picked services. The real server renders them with the code of
// `mirrord up init`.
const renderYaml = (services) =>
  "services:\n" +
  services
    .map((s) => {
      const lines = [`  ${s.name}:`];
      lines.push(
        s.target
          ? `    target:\n      path: ${s.target.path}`
          : "    target: none",
      );
      if (s.mode !== "split") lines.push(`    default_mode: ${s.mode}`);
      if (s.header_filter)
        lines.push(
          `    http_filter:\n      header_filter: "${s.header_filter}"`,
        );
      if (s.ignore_ports.length)
        lines.push(`    ignore_ports: [${s.ignore_ports.join(", ")}]`);
      lines.push("    run:");
      if (s.directory) lines.push(`      directory: ${s.directory}`);
      lines.push(
        `      command: ${JSON.stringify(s.command.trim().split(/\s+/))}`,
      );
      return lines.join("\n");
    })
    .join("\n") +
  "\n";

const readBody = (req) =>
  new Promise((resolve) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => resolve(body));
  });

const server = http.createServer(async (req, res) => {
  res.setHeader("Content-Type", "application/json");
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type");

  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url, `http://${req.headers.host}`);
  const pathname = url.pathname;

  console.log(`[Mock API] ${req.method} ${pathname}`);

  const send = (status, body) => {
    res.writeHead(status);
    res.end(typeof body === "string" ? body : JSON.stringify(body));
  };

  switch (pathname) {
    case "/api/v1/is-returning":
      return send(200, "false");
    case "/api/v1/up/modes":
      return send(200, { modes: mockData.modes });
    case "/api/v1/up/render": {
      const { services } = JSON.parse(await readBody(req));
      return send(200, { yaml: renderYaml(services) });
    }
    case "/api/v2/kube/contexts":
      return send(200, mockData.contexts);
    case "/api/v2/kube/namespaces":
      return send(200, { namespaces: mockData.namespaces });
    case "/api/v1/up/workloads": {
      const namespace = url.searchParams.get("namespace") ?? "default";
      const mode = url.searchParams.get("mode");
      const kinds =
        mockData.modes.find((m) => m.id === mode)?.target_types ?? [];
      const workloads = (mockData.targets[namespace] || [])
        .map((t) => t.target_path)
        .filter((path) => kinds.includes(path.split("/")[0]));
      return send(200, { workloads });
    }
    case "/api/v1/up/ports": {
      const namespace = url.searchParams.get("namespace") ?? "default";
      const path = url.searchParams.get("path");
      const target = (mockData.targets[namespace] || []).find(
        (t) => t.target_path === path,
      );
      return send(200, { ports: target?.detected_ports ?? [] });
    }
    default:
      return send(404, { error: "Not found" });
  }
});

const PORT = 3000;
server.listen(PORT, () => {
  console.log(`Mock API server running at http://localhost:${PORT}`);
  console.log("Available endpoints:");
  console.log("  GET  /api/v1/is-returning");
  console.log("  GET  /api/v1/up/modes");
  console.log("  GET  /api/v1/up/workloads?namespace=&mode=");
  console.log("  GET  /api/v1/up/ports?namespace=&path=");
  console.log("  POST /api/v1/up/render");
  console.log("  GET  /api/v2/kube/contexts");
  console.log("  GET  /api/v2/kube/namespaces");
});
