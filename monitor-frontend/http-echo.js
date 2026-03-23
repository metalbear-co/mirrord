const http = require('http');
const fs = require('fs');
const dns = require('dns');

function log(type, msg) {
  console.log(`[${new Date().toISOString()}] [${type}] ${msg}`);
}

const server = http.createServer((req, res) => {
  log('HTTP', `${req.method} ${req.url} from ${req.headers.host || 'unknown'}`);

  // Log headers
  const interesting = ['user-agent', 'referer', 'content-type', 'authorization'];
  for (const h of interesting) {
    if (req.headers[h]) {
      log('HTTP', `  ${h}: ${req.headers[h].substring(0, 100)}`);
    }
  }

  // Read some remote files to generate file events
  try {
    const hostname = fs.readFileSync('/etc/hostname', 'utf-8').trim();
    log('FILE', `read /etc/hostname: ${hostname}`);
  } catch {}

  // Resolve DNS
  dns.resolve4('kubernetes.default.svc.cluster.local', (err, addrs) => {
    if (!err) log('DNS', `kubernetes.default -> ${addrs.join(', ')}`);
  });

  res.writeHead(200, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify({
    message: 'mirrord session monitor echo',
    method: req.method,
    url: req.url,
    timestamp: new Date().toISOString(),
  }));
});

server.listen(4000, '0.0.0.0', () => {
  log('INFO', 'Echo server listening on 0.0.0.0:4000');
  log('INFO', `PID: ${process.pid}`);

  // Log environment
  const envKeys = Object.keys(process.env)
    .filter(k => !k.startsWith('_') && !k.startsWith('npm') && !k.startsWith('HOME'))
    .filter(k => k.includes('METALBEAR') || k.includes('KUBERNETES') || k.includes('APP_'))
    .slice(0, 10);
  if (envKeys.length > 0) {
    log('ENV', `remote env vars: ${envKeys.join(', ')}`);
  }
});

process.on('SIGINT', () => {
  log('INFO', 'Shutting down');
  server.close();
  process.exit(0);
});
