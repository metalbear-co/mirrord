import * as http from 'node:http'
import * as fs from 'node:fs'
import * as path from 'node:path'
import * as os from 'node:os'
import express from 'express'
import cors from 'cors'
import { WebSocketServer, WebSocket } from 'ws'
import type { SessionInfo, WsMessage } from './types.js'

const SESSIONS_DIR = path.join(os.homedir(), '.mirrord', 'sessions')
const PORT = 59281

// Track known sessions
const knownSessions = new Map<string, SessionInfo>()

function sessionIdFromSocket(filename: string): string {
  return filename.replace('.sock', '')
}

/** Make an HTTP request over a Unix socket */
function requestOverSocket(
  socketPath: string,
  method: string,
  reqPath: string,
): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        socketPath,
        path: reqPath,
        method,
        headers: { 'Content-Type': 'application/json' },
      },
      (res) => {
        let body = ''
        res.on('data', (chunk) => (body += chunk))
        res.on('end', () => resolve({ status: res.statusCode ?? 500, body }))
      },
    )
    req.on('error', reject)
    req.setTimeout(3000, () => {
      req.destroy(new Error('timeout'))
    })
    req.end()
  })
}

/** Fetch session info from a socket */
async function fetchSessionInfo(socketPath: string): Promise<SessionInfo | null> {
  try {
    const { status, body } = await requestOverSocket(socketPath, 'GET', '/info')
    if (status === 200) return JSON.parse(body)
  } catch {
    // Socket not ready or dead
  }
  return null
}

/** Discover all .sock files and fetch their info */
async function discoverSessions(): Promise<void> {
  let files: string[] = []
  try {
    files = fs.readdirSync(SESSIONS_DIR).filter((f) => f.endsWith('.sock'))
  } catch {
    return
  }

  const currentIds = new Set(files.map(sessionIdFromSocket))

  // Remove sessions whose sockets are gone
  for (const id of knownSessions.keys()) {
    if (!currentIds.has(id)) {
      knownSessions.delete(id)
      broadcast({ type: 'session_removed', session_id: id })
      console.log(`[server] Session removed: ${id}`)
    }
  }

  // Add new sessions
  for (const file of files) {
    const id = sessionIdFromSocket(file)
    if (!knownSessions.has(id)) {
      const socketPath = path.join(SESSIONS_DIR, file)
      const info = await fetchSessionInfo(socketPath)
      if (info) {
        knownSessions.set(id, info)
        broadcast({ type: 'session_added', session_id: id, info })
        console.log(`[server] Session discovered: ${id} (${info.target})`)
      }
    }
  }
}

// WebSocket clients
const wsClients = new Set<WebSocket>()

function broadcast(msg: WsMessage) {
  const data = JSON.stringify(msg)
  for (const client of wsClients) {
    if (client.readyState === WebSocket.OPEN) {
      client.send(data)
    }
  }
}

// Express app
const app = express()
app.use(cors())
app.use(express.json())

app.get('/api/health', (_req, res) => {
  res.json({ status: 'ok', sessions: knownSessions.size })
})

app.get('/api/sessions', (_req, res) => {
  res.json(Array.from(knownSessions.values()))
})

app.get('/api/sessions/:id', (req, res) => {
  const info = knownSessions.get(req.params.id)
  if (!info) return res.status(404).json({ error: 'Session not found' })
  res.json(info)
})

app.post('/api/sessions/:id/kill', async (req, res) => {
  const id = req.params.id
  if (!knownSessions.has(id)) return res.status(404).json({ error: 'Session not found' })

  const socketPath = path.join(SESSIONS_DIR, `${id}.sock`)
  try {
    const { status, body } = await requestOverSocket(socketPath, 'POST', '/kill')
    knownSessions.delete(id)
    broadcast({ type: 'session_removed', session_id: id })
    res.status(status).json(JSON.parse(body))
  } catch (err) {
    res.status(500).json({ error: 'Failed to kill session' })
  }
})

app.get('/api/sessions/:id/events', (req, res) => {
  const id = req.params.id
  if (!knownSessions.has(id)) return res.status(404).json({ error: 'Session not found' })

  const socketPath = path.join(SESSIONS_DIR, `${id}.sock`)

  // Proxy SSE stream from the session socket
  const proxyReq = http.request(
    { socketPath, path: '/events', method: 'GET' },
    (proxyRes) => {
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        Connection: 'keep-alive',
      })
      proxyRes.pipe(res)
    },
  )

  proxyReq.on('error', () => {
    if (!res.headersSent) {
      res.status(502).json({ error: 'Failed to connect to session' })
    }
  })

  req.on('close', () => proxyReq.destroy())
  proxyReq.end()
})

// Serve static frontend files
const frontendDist = path.join(__dirname || process.cwd(), 'frontend', 'dist')
app.use(express.static(frontendDist))
app.get('*', (req, res, next) => {
  if (req.path.startsWith('/api') || req.path.startsWith('/ws')) return next()
  const indexPath = path.join(frontendDist, 'index.html')
  if (fs.existsSync(indexPath)) {
    res.sendFile(indexPath)
  } else {
    next()
  }
})

// Create HTTP server and attach WebSocket
const server = http.createServer(app)
const wss = new WebSocketServer({ server, path: '/ws' })

wss.on('connection', (ws) => {
  wsClients.add(ws)
  console.log(`[server] WebSocket client connected (${wsClients.size} total)`)

  // Send current sessions on connect
  for (const info of knownSessions.values()) {
    ws.send(JSON.stringify({ type: 'session_added', session_id: info.session_id, info }))
  }

  ws.on('close', () => {
    wsClients.delete(ws)
    console.log(`[server] WebSocket client disconnected (${wsClients.size} total)`)
  })
})

// Watch sessions directory for changes
fs.mkdirSync(SESSIONS_DIR, { recursive: true })

let debounceTimer: ReturnType<typeof setTimeout> | null = null
fs.watch(SESSIONS_DIR, () => {
  if (debounceTimer) clearTimeout(debounceTimer)
  debounceTimer = setTimeout(() => discoverSessions(), 500)
})

// Initial discovery + start
async function main() {
  await discoverSessions()

  server.listen(PORT, () => {
    console.log(`[server] Aggregator running on http://localhost:${PORT}`)
    console.log(`[server] WebSocket at ws://localhost:${PORT}/ws`)
    console.log(`[server] Watching ${SESSIONS_DIR} for session sockets`)
  })
}

main()
