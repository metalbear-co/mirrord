import http from 'http';

const mockData = {
  clusterDetails: {
    namespaces: ['default', 'production', 'staging', 'development'],
    target_types: ['deployment', 'pod', 'statefulset', 'rollout'],
  },
  targets: {
    default: [
      { target_path: 'deployment/api-server', target_namespace: 'default', detected_ports: [8080, 3000] },
      { target_path: 'deployment/web-frontend', target_namespace: 'default', detected_ports: [80, 443] },
      { target_path: 'pod/redis-master-0', target_namespace: 'default', detected_ports: [6379] },
      { target_path: 'statefulset/postgres', target_namespace: 'default', detected_ports: [5432] },
    ],
    production: [
      { target_path: 'deployment/prod-api', target_namespace: 'production', detected_ports: [8080] },
      { target_path: 'deployment/prod-worker', target_namespace: 'production', detected_ports: [9090] },
      { target_path: 'pod/prod-api-abc123', target_namespace: 'production', detected_ports: [8080] },
      { target_path: 'pod/prod-worker-xyz789', target_namespace: 'production', detected_ports: [9090] },
      { target_path: 'rollout/prod-canary', target_namespace: 'production', detected_ports: [8080] },
    ],
    staging: [
      { target_path: 'deployment/staging-api', target_namespace: 'staging', detected_ports: [8080, 9090] },
      { target_path: 'deployment/staging-web', target_namespace: 'staging', detected_ports: [3000] },
    ],
    development: [
      { target_path: 'deployment/dev-api', target_namespace: 'development', detected_ports: [8080] },
      { target_path: 'pod/dev-debug', target_namespace: 'development', detected_ports: [5005] },
    ],
  },
};

const server = http.createServer((req, res) => {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');

  if (req.method === 'OPTIONS') {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url, `http://${req.headers.host}`);
  const pathname = url.pathname;

  console.log(`[Mock API] ${req.method} ${pathname}`);

  if (pathname === '/api/v1/is-returning') {
    res.writeHead(200);
    res.end(JSON.stringify({ isReturning: false }));
    return;
  }

  if (pathname === '/api/v1/cluster-details') {
    res.writeHead(200);
    res.end(JSON.stringify(mockData.clusterDetails));
    return;
  }

  const targetsMatch = pathname.match(/^\/api\/v1\/namespace\/([^/]+)\/targets$/);
  if (targetsMatch) {
    const namespace = targetsMatch[1];
    const targetType = url.searchParams.get('target_type');

    let targets = mockData.targets[namespace] || [];

    if (targetType) {
      targets = targets.filter(t => t.target_path.startsWith(targetType + '/'));
    }

    res.writeHead(200);
    res.end(JSON.stringify(targets));
    return;
  }

  res.writeHead(404);
  res.end(JSON.stringify({ error: 'Not found' }));
});

const PORT = 3000;
server.listen(PORT, () => {
  console.log(`Mock API server running at http://localhost:${PORT}`);
  console.log('Available endpoints:');
  console.log('  GET /api/v1/is-returning');
  console.log('  GET /api/v1/cluster-details');
  console.log('  GET /api/v1/namespace/:namespace/targets');
});
