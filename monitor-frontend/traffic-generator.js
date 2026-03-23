// This script runs under mirrord and generates real traffic events
// by reading remote files, resolving DNS, and making HTTP requests
// that go through the mirrord layer

const fs = require('fs');
const dns = require('dns');
const http = require('http');
const https = require('https');

const INTERVAL = 3000; // every 3 seconds

function log(type, msg) {
  const ts = new Date().toISOString();
  console.log(`[${ts}] [${type}] ${msg}`);
}

async function readRemoteFile(path) {
  try {
    const content = fs.readFileSync(path, 'utf-8');
    log('FILE', `read ${path} (${content.length} bytes)`);
    return content;
  } catch (e) {
    log('FILE', `failed to read ${path}: ${e.message}`);
    return null;
  }
}

function resolveDns(hostname) {
  return new Promise((resolve) => {
    dns.resolve4(hostname, (err, addresses) => {
      if (err) {
        log('DNS', `resolve ${hostname} failed: ${err.message}`);
        resolve(null);
      } else {
        log('DNS', `resolve ${hostname} -> ${addresses.join(', ')}`);
        resolve(addresses);
      }
    });
  });
}

function httpGet(url) {
  return new Promise((resolve) => {
    const client = url.startsWith('https') ? https : http;
    const req = client.get(url, { timeout: 5000 }, (res) => {
      let data = '';
      res.on('data', (chunk) => data += chunk);
      res.on('end', () => {
        log('HTTP', `GET ${url} -> ${res.statusCode} (${data.length} bytes)`);
        resolve({ status: res.statusCode, size: data.length });
      });
    });
    req.on('error', (e) => {
      log('HTTP', `GET ${url} failed: ${e.message}`);
      resolve(null);
    });
    req.on('timeout', () => {
      log('HTTP', `GET ${url} timeout`);
      req.destroy();
      resolve(null);
    });
  });
}

// Files to try reading (these exist on most K8s pods)
const FILES = [
  '/etc/hostname',
  '/etc/resolv.conf',
  '/etc/hosts',
  '/etc/os-release',
  '/proc/1/status',
  '/etc/passwd',
];

// Hostnames to resolve (cluster-internal DNS)
const HOSTNAMES = [
  'kubernetes.default.svc.cluster.local',
  'kube-dns.kube-system.svc.cluster.local',
  'app-metalbear-co.default.svc.cluster.local',
  'crm.default.svc.cluster.local',
];

// URLs to fetch
const URLS = [
  'http://localhost:4000/api/v1/frontegg',
  'http://kubernetes.default.svc.cluster.local:443',
];

let iteration = 0;

async function tick() {
  iteration++;
  log('INFO', `--- iteration ${iteration} ---`);

  // Rotate through different activities
  const fileIdx = iteration % FILES.length;
  const hostIdx = iteration % HOSTNAMES.length;

  await readRemoteFile(FILES[fileIdx]);
  await resolveDns(HOSTNAMES[hostIdx]);

  if (iteration % 3 === 0) {
    const urlIdx = (iteration / 3) % URLS.length;
    await httpGet(URLS[Math.floor(urlIdx)]);
  }

  // Read env vars (mirrord injects remote env)
  if (iteration % 5 === 0) {
    const envKeys = Object.keys(process.env).filter(k =>
      !k.startsWith('_') && !k.startsWith('npm') && !k.startsWith('HOME')
    ).slice(0, 5);
    log('ENV', `remote env vars: ${envKeys.join(', ')}`);
  }
}

log('INFO', 'Traffic generator started. Generating events every 3s...');
log('INFO', `PID: ${process.pid}`);

// Initial burst
tick().then(() => {
  setInterval(tick, INTERVAL);
});

// Keep alive
process.on('SIGINT', () => {
  log('INFO', 'Shutting down');
  process.exit(0);
});
