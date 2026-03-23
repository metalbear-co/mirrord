import * as http from 'node:http'
import * as fs from 'node:fs'
import * as path from 'node:path'
import * as os from 'node:os'
import type { SessionInfo, SessionEvent } from './types.js'

const SESSIONS_DIR = path.join(os.homedir(), '.mirrord', 'sessions')

const sessions: SessionInfo[] = [
  {
    session_id: 'sess-a1b2c3',
    target: 'deploy/checkout-svc',
    mode: 'steal',
    pid: 12001,
    process_name: 'node',
    started_at: new Date(Date.now() - 300_000).toISOString(),
    ports: [8080, 3000],
    mirrord_version: '3.142.0',
  },
  {
    session_id: 'sess-d4e5f6',
    target: 'deploy/payments-svc',
    mode: 'mirror',
    pid: 12002,
    process_name: 'python',
    started_at: new Date(Date.now() - 120_000).toISOString(),
    ports: [5000],
    mirrord_version: '3.142.0',
  },
  {
    session_id: 'sess-g7h8i9',
    target: 'deploy/auth-svc',
    mode: 'steal',
    pid: 12003,
    process_name: 'java',
    started_at: new Date(Date.now() - 600_000).toISOString(),
    ports: [8443, 9090],
    mirrord_version: '3.141.0',
  },
]

function randomEvent(): SessionEvent {
  const types: SessionEvent['type'][] = ['file_op', 'dns_query', 'http_request', 'connection', 'env_var']
  const t = types[Math.floor(Math.random() * types.length)]
  const base = { type: t, timestamp: Date.now() }

  switch (t) {
    case 'file_op':
      return {
        ...base,
        data: {
          op: ['read', 'write', 'open'][Math.floor(Math.random() * 3)],
          path: ['/etc/config.yaml', '/tmp/cache.json', '/var/log/app.log'][Math.floor(Math.random() * 3)],
          bytes: Math.floor(Math.random() * 4096),
        },
      }
    case 'dns_query':
      return {
        ...base,
        data: {
          name: ['api.internal.svc', 'redis.default.svc', 'postgres.db.svc'][Math.floor(Math.random() * 3)],
          record_type: 'A',
          resolved: `10.0.${Math.floor(Math.random() * 255)}.${Math.floor(Math.random() * 255)}`,
        },
      }
    case 'http_request':
      return {
        ...base,
        data: {
          method: ['GET', 'POST', 'PUT', 'DELETE'][Math.floor(Math.random() * 4)],
          path: ['/api/orders', '/api/users', '/health', '/api/payments'][Math.floor(Math.random() * 4)],
          status: [200, 201, 404, 500][Math.floor(Math.random() * 4)],
          duration_ms: Math.floor(Math.random() * 500),
        },
      }
    case 'connection':
      return {
        ...base,
        data: {
          remote: `10.0.${Math.floor(Math.random() * 255)}.${Math.floor(Math.random() * 255)}:${3000 + Math.floor(Math.random() * 5000)}`,
          direction: Math.random() > 0.5 ? 'incoming' : 'outgoing',
          protocol: Math.random() > 0.3 ? 'TCP' : 'UDP',
        },
      }
    case 'env_var':
      return {
        ...base,
        data: {
          name: ['DATABASE_URL', 'REDIS_HOST', 'API_KEY', 'LOG_LEVEL'][Math.floor(Math.random() * 4)],
          action: 'read',
        },
      }
  }
}

function createSessionServer(session: SessionInfo): http.Server {
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/health') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      res.end(JSON.stringify({ status: 'ok' }))
      return
    }

    if (req.method === 'GET' && req.url === '/info') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      res.end(JSON.stringify(session))
      return
    }

    if (req.method === 'GET' && req.url === '/events') {
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        Connection: 'keep-alive',
      })

      const interval = setInterval(() => {
        const event = randomEvent()
        res.write(`data: ${JSON.stringify(event)}\n\n`)
      }, 2000)

      req.on('close', () => clearInterval(interval))
      return
    }

    if (req.method === 'POST' && req.url === '/kill') {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      res.end(JSON.stringify({ status: 'killed', session_id: session.session_id }))

      const socketPath = path.join(SESSIONS_DIR, `${session.session_id}.sock`)
      setTimeout(() => {
        server.close()
        try { fs.unlinkSync(socketPath) } catch {}
        console.log(`[mock] Session ${session.session_id} killed, socket removed`)
      }, 100)
      return
    }

    res.writeHead(404)
    res.end('Not found')
  })

  return server
}

// Ensure sessions directory exists
fs.mkdirSync(SESSIONS_DIR, { recursive: true })

const servers: http.Server[] = []

for (const session of sessions) {
  const socketPath = path.join(SESSIONS_DIR, `${session.session_id}.sock`)

  // Clean up stale socket
  try { fs.unlinkSync(socketPath) } catch {}

  const server = createSessionServer(session)
  server.listen(socketPath, () => {
    console.log(`[mock] Session ${session.session_id} (${session.target}) listening on ${socketPath}`)
  })
  servers.push(server)
}

function cleanup() {
  console.log('\n[mock] Cleaning up sockets...')
  for (const session of sessions) {
    const socketPath = path.join(SESSIONS_DIR, `${session.session_id}.sock`)
    try { fs.unlinkSync(socketPath) } catch {}
  }
  for (const server of servers) {
    server.close()
  }
  process.exit(0)
}

process.on('SIGINT', cleanup)
process.on('SIGTERM', cleanup)
process.on('exit', () => {
  for (const session of sessions) {
    const socketPath = path.join(SESSIONS_DIR, `${session.session_id}.sock`)
    try { fs.unlinkSync(socketPath) } catch {}
  }
})

console.log(`[mock] ${sessions.length} mock sessions running. Press Ctrl+C to stop.`)
