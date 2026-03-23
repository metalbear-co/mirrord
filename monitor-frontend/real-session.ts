import http from 'node:http'
import net from 'node:net'
import fs from 'node:fs'
import path from 'node:path'
import { spawn, ChildProcess } from 'node:child_process'
import crypto from 'node:crypto'
import type { SessionInfo, SessionEvent } from './types'

const SESSIONS_DIR = path.join(process.env.HOME || '~', '.mirrord', 'sessions')
const SESSION_ID = `sess-${crypto.randomBytes(4).toString('hex')}`
const SOCKET_PATH = path.join(SESSIONS_DIR, `${SESSION_ID}.sock`)

// Parse args: supports two forms:
//   tsx real-session.ts <target> [cmd args...]
//   tsx real-session.ts --config-file <path> [cmd args...]
let CONFIG_FILE: string | undefined
let TARGET: string
let rawCmdArgs: string[]

if (process.argv[2] === '--config-file') {
  CONFIG_FILE = process.argv[3]
  // Read target from config file
  try {
    const cfg = JSON.parse(fs.readFileSync(CONFIG_FILE, 'utf-8'))
    TARGET = cfg.target?.path || cfg.target || 'unknown'
  } catch {
    TARGET = 'unknown'
  }
  rawCmdArgs = process.argv.slice(4)
} else {
  TARGET = process.argv[2] || 'deployment/app-metalbear-co'
  rawCmdArgs = process.argv.slice(3)
}
const MODE = 'mirror'

const sessionInfo: SessionInfo = {
  session_id: SESSION_ID,
  target: TARGET,
  mode: MODE as 'steal' | 'mirror',
  pid: 0, // will be set when mirrord starts
  process_name: 'sleep',
  started_at: new Date().toISOString(),
  ports: [],
  mirrord_version: '',
}

// Get mirrord version
try {
  const { execSync } = require('node:child_process')
  const version = execSync('mirrord --version 2>/dev/null').toString().trim()
  sessionInfo.mirrord_version = version.replace('mirrord ', '')
} catch {
  sessionInfo.mirrord_version = 'unknown'
}

// Create sessions directory
fs.mkdirSync(SESSIONS_DIR, { recursive: true, mode: 0o700 })

// Clean up stale socket
if (fs.existsSync(SOCKET_PATH)) fs.unlinkSync(SOCKET_PATH)

// Track events from mirrord output
const events: SessionEvent[] = []
function addEvent(type: SessionEvent['type'], data: Record<string, unknown>) {
  const event: SessionEvent = { type, timestamp: Date.now(), data }
  events.push(event)
  if (events.length > 200) events.shift()
}

// Start the Unix socket HTTP server
const server = http.createServer((req, res) => {
  res.setHeader('Content-Type', 'application/json')

  if (req.method === 'GET' && req.url === '/health') {
    res.end(JSON.stringify({ status: 'ok' }))
    return
  }

  if (req.method === 'GET' && req.url === '/info') {
    res.end(JSON.stringify(sessionInfo))
    return
  }

  if (req.method === 'GET' && req.url === '/events') {
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      Connection: 'keep-alive',
    })

    // Send existing events
    for (const event of events) {
      res.write(`data: ${JSON.stringify(event)}\n\n`)
    }

    // Only send NEW events (track cursor)
    let sentCount = events.length
    const interval = setInterval(() => {
      if (events.length > sentCount) {
        const newEvents = events.slice(sentCount)
        for (const event of newEvents) {
          res.write(`data: ${JSON.stringify(event)}\n\n`)
        }
        sentCount = events.length
      }
    }, 1000)

    req.on('close', () => clearInterval(interval))
    return
  }

  if (req.method === 'POST' && req.url === '/kill') {
    res.end(JSON.stringify({ status: 'killing' }))
    cleanup()
    return
  }

  res.writeHead(404)
  res.end(JSON.stringify({ error: 'not found' }))
})

server.listen(SOCKET_PATH, () => {
  fs.chmodSync(SOCKET_PATH, 0o600)
  console.log(`[real] Socket server listening on ${SOCKET_PATH}`)
})

const CMD = rawCmdArgs[0] || 'node'
const CMD_ARGS = rawCmdArgs.length > 1 ? rawCmdArgs.slice(1) : (rawCmdArgs[0] ? [] : [path.join(__dirname, 'traffic-generator.js')])

const mirrordArgs = CONFIG_FILE
  ? ['exec', '--config-file', CONFIG_FILE, '--', CMD, ...CMD_ARGS]
  : ['exec', '--target', TARGET, '--', CMD, ...CMD_ARGS]

console.log(`[real] Starting mirrord ${mirrordArgs.join(' ')} ...`)

const mirrordProcess: ChildProcess = spawn('mirrord', mirrordArgs, {
  env: {
    ...process.env,
    MIRRORD_AGENT_RUST_LOG: 'info',
    RUST_LOG: 'mirrord=info',
  },
  stdio: ['pipe', 'pipe', 'pipe'],
})

sessionInfo.process_name = CMD

sessionInfo.pid = mirrordProcess.pid || 0

addEvent('connection', {
  message: `mirrord session started targeting ${TARGET}`,
  mode: MODE,
})

mirrordProcess.stdout?.on('data', (data: Buffer) => {
  const lines = data.toString().trim().split('\n')
  for (const line of lines) {
    if (!line.trim()) continue
    console.log(`[mirrord stdout] ${line}`)

    // Parse structured lines from traffic-generator: [timestamp] [TYPE] message
    const typeMatch = line.match(/\[[\dT:.Z-]+\]\s+\[(FILE|DNS|HTTP|ENV|INFO)\]\s+(.+)/)
    if (typeMatch) {
      const [, type, message] = typeMatch
      const eventType = {
        FILE: 'file_op',
        DNS: 'dns_query',
        HTTP: 'http_request',
        ENV: 'env_var',
        INFO: 'connection',
      }[type] || 'connection'
      addEvent(eventType, { message, source: 'app' })
    } else {
      addEvent('connection', { message: line, source: 'stdout' })
    }
  }
})

mirrordProcess.stderr?.on('data', (data: Buffer) => {
  const lines = data.toString().trim().split('\n')
  for (const line of lines) {
    if (!line) continue
    console.log(`[mirrord stderr] ${line}`)

    // Parse interesting events from mirrord output
    if (line.includes('file') || line.includes('open')) {
      addEvent('file_op', { message: line, operation: 'read' })
    } else if (line.includes('dns') || line.includes('DNS') || line.includes('resolve')) {
      addEvent('dns_query', { message: line })
    } else if (line.includes('connect') || line.includes('port') || line.includes('traffic')) {
      addEvent('connection', { message: line })
    } else if (line.includes('env')) {
      addEvent('env_var', { message: line })
    } else {
      addEvent('connection', { message: line, source: 'stderr' })
    }
  }
})

mirrordProcess.on('exit', (code) => {
  console.log(`[real] mirrord process exited with code ${code}`)
  addEvent('connection', { message: `mirrord exited with code ${code}` })
})

mirrordProcess.on('error', (err) => {
  console.error(`[real] mirrord process error:`, err.message)
  addEvent('connection', { message: `error: ${err.message}` })
})

// Cleanup
function cleanup() {
  console.log('[real] Cleaning up...')
  try { mirrordProcess.kill() } catch {}
  try { fs.unlinkSync(SOCKET_PATH) } catch {}
  try { server.close() } catch {}
  process.exit(0)
}

process.on('SIGINT', cleanup)
process.on('SIGTERM', cleanup)
process.on('exit', () => {
  try { fs.unlinkSync(SOCKET_PATH) } catch {}
})

console.log(`[real] Session ${SESSION_ID} running. Target: ${TARGET}, Mode: ${MODE}`)
console.log(`[real] Press Ctrl+C to stop.`)
