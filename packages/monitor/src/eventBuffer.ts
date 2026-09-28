import type { SubscribeEventRow } from './subscribeEvents'

/** Rows kept in memory. Beyond this the oldest are dropped. */
export const MAX_ROWS = 10_000

// Request ids are unique only within their connection, and connections only within their session.
function pairKey(row: SubscribeEventRow): string {
  return `${row.sessionKey} ${row.correlation}`
}

/**
 * Collects event rows in arrival order, pairing each HTTP response with the request it answers and
 * dropping the oldest rows past {@link MAX_ROWS}.
 */
export class EventBuffer {
  private rows: SubscribeEventRow[] = []

  // Absolute position (counted from the first row ever collected, so eviction does not shift it)
  // of each request still awaiting its response.
  private openRequests = new Map<string, number>()
  private dropped = 0

  /**
   * Adds `incoming`, returning the resulting rows.
   *
   * A response completes the row its request opened. One whose request has been dropped, or that
   * carries no ids, keeps its own row.
   */
  absorb(incoming: SubscribeEventRow[]): SubscribeEventRow[] {
    const next = this.rows.slice()

    for (const row of incoming) {
      if (row.type === 'http_response' && row.correlation !== '') {
        const at = this.openRequests.get(pairKey(row))
        const index = at === undefined ? -1 : at - this.dropped
        const request = index >= 0 ? next[index] : undefined

        if (request?.awaitingResponse) {
          next[index] = {
            ...request,
            status: row.status,
            awaitingResponse: false,
          }
          this.openRequests.delete(pairKey(row))
          continue
        }
      }

      if (row.awaitingResponse && row.correlation !== '') {
        this.openRequests.set(pairKey(row), this.dropped + next.length)
      }

      next.push(row)
    }

    this.rows = next.length > MAX_ROWS ? this.evict(next) : next

    return this.rows
  }

  clear(): void {
    this.rows = []
    this.openRequests = new Map()
    this.dropped = 0
  }

  private evict(rows: SubscribeEventRow[]): SubscribeEventRow[] {
    const count = rows.length - MAX_ROWS

    for (let index = 0; index < count; index += 1) {
      const row = rows[index]
      // A reused key points at the newer request, which stays.
      if (row && this.openRequests.get(pairKey(row)) === this.dropped + index) {
        this.openRequests.delete(pairKey(row))
      }
    }
    this.dropped += count

    return rows.slice(count)
  }
}
