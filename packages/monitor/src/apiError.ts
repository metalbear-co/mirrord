/**
 * A non-ok response from the local API. Distinguishes "the server answered and refused" from
 * a `fetch` rejection, which means the browser never reached the server at all.
 */
export class ApiError extends Error {
  readonly status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}
