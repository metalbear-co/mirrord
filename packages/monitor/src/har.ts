import type { Cookie, Har, Header, QueryString } from 'har-format'
import type { MonitorEvent, SessionInfo } from './types'
import { EventType } from './eventTypes'

interface TimestampedEvent {
  event: MonitorEvent
  receivedAt: Date
}

type HeaderPair = [string, string]

interface Exchange {
  request?: Extract<MonitorEvent, { type: 'incoming_request' }>
  requestBody?: Extract<MonitorEvent, { type: 'incoming_request_body' }>
  response?: Extract<MonitorEvent, { type: 'incoming_response' }>
  responseBody?: Extract<MonitorEvent, { type: 'incoming_response_body' }>
  startedAt?: Date
  endedAt?: Date
}

const toHarHeaders = (headers?: HeaderPair[]): Header[] =>
  (headers ?? []).map(([name, value]) => ({ name, value }))

const findHeader = (headers: HeaderPair[] | undefined, name: string): string | undefined =>
  headers?.find(([key]) => key.toLowerCase() === name.toLowerCase())?.[1]

const parseQueryString = (path: string): QueryString[] => {
  const idx = path.indexOf('?')
  if (idx === -1) return []
  const params = new URLSearchParams(path.slice(idx + 1))
  return Array.from(params.entries()).map(([name, value]) => ({ name, value }))
}

const parseCookiePairs = (values: string[]): Cookie[] =>
  values
    .map((value) => value.trim())
    .filter((value) => value.includes('='))
    .map((value) => {
      const idx = value.indexOf('=')
      return { name: value.slice(0, idx), value: value.slice(idx + 1) }
    })

const requestCookies = (headers?: HeaderPair[]): Cookie[] =>
  parseCookiePairs(findHeader(headers, 'cookie')?.split(';') ?? [])

const responseCookies = (headers?: HeaderPair[]): Cookie[] =>
  parseCookiePairs(
    (headers ?? [])
      .filter(([name]) => name.toLowerCase() === 'set-cookie')
      .map(([, value]) => value.split(';')[0] ?? ''),
  )

// Bodies were captured up to a byte cap; a truncated body isn't a faithful HAR entry, so we mark
// it with a comment and still include what we have.
const buildContent = (
  body: string | undefined,
  bytes: number | undefined,
  mimeType: string | undefined,
  truncated: boolean | undefined,
) => ({
  // -1 marks the size as unknown: without a body event we cannot distinguish an empty body
  // from body capture being disabled.
  size: bytes ?? -1,
  mimeType: mimeType ?? 'application/octet-stream',
  text: body ?? '',
  ...(truncated ? { comment: 'body truncated by mirrord session monitor capture limit' } : {}),
})

// Groups the flat monitor event stream by its per-exchange correlation id and emits a HAR 1.2
// document, pairing each request with its response (and their bodies) into one entry.
export function buildHar(session: SessionInfo, events: TimestampedEvent[]): Har {
  const exchanges = new Map<string, Exchange>()

  for (const { event, receivedAt } of events) {
    if (
      event.type !== EventType.IncomingRequest &&
      event.type !== EventType.IncomingResponse &&
      event.type !== EventType.IncomingRequestBody &&
      event.type !== EventType.IncomingResponseBody
    ) {
      continue
    }
    const ex = exchanges.get(event.id) ?? {}
    switch (event.type) {
      case EventType.IncomingRequest:
        ex.request = event
        ex.startedAt = receivedAt
        break
      case EventType.IncomingRequestBody:
        ex.requestBody = event
        break
      case EventType.IncomingResponse:
        ex.response = event
        ex.endedAt = receivedAt
        break
      case EventType.IncomingResponseBody:
        ex.responseBody = event
        ex.endedAt = receivedAt
        break
    }
    exchanges.set(event.id, ex)
  }

  const entries = Array.from(exchanges.values())
    .filter((ex) => ex.request)
    .map((ex) => {
      const req = ex.request!
      const startedAt = ex.startedAt ?? new Date(0)
      const time =
        ex.startedAt && ex.endedAt ? Math.max(0, ex.endedAt.getTime() - ex.startedAt.getTime()) : 0
      const reqMime = findHeader(req.headers, 'content-type')
      const resMime = findHeader(ex.response?.headers, 'content-type')
      const scheme = req.port === 443 ? 'https' : 'http'

      return {
        startedDateTime: startedAt.toISOString(),
        time,
        request: {
          method: req.method,
          url: `${scheme}://${req.host}${req.path}`,
          httpVersion: req.http_version ?? 'HTTP/1.1',
          headers: toHarHeaders(req.headers),
          queryString: parseQueryString(req.path),
          cookies: requestCookies(req.headers),
          headersSize: -1,
          bodySize: ex.requestBody?.bytes ?? -1,
          ...(ex.requestBody
            ? {
                postData: {
                  mimeType: reqMime ?? 'application/octet-stream',
                  text: ex.requestBody.body,
                },
              }
            : {}),
        },
        response: {
          status: ex.response?.status ?? 0,
          statusText: '',
          httpVersion: ex.response?.http_version ?? 'HTTP/1.1',
          headers: toHarHeaders(ex.response?.headers),
          cookies: responseCookies(ex.response?.headers),
          content: buildContent(
            ex.responseBody?.body,
            ex.responseBody?.bytes,
            resMime,
            ex.responseBody?.truncated,
          ),
          redirectURL: findHeader(ex.response?.headers, 'location') ?? '',
          headersSize: -1,
          bodySize: ex.responseBody?.bytes ?? -1,
        },
        cache: {},
        timings: { send: 0, wait: time, receive: 0 },
        comment: `mirrord session ${session.session_id} · target ${session.target}`,
      }
    })

  return {
    log: {
      version: '1.2',
      creator: { name: 'mirrord session monitor', version: session.mirrord_version },
      entries,
    },
  }
}
