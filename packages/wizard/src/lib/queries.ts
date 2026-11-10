import { useQuery } from '@tanstack/react-query'
import ALL_API_ROUTES from './routes'
import type { ModeId, ModeInfo, PickedService, PickedWorkload } from './up'

const QUERY_STALE_TIME_MS = 30000
const FIRST_SERVER_ERROR = 500

const url = (route: string) => window.location.origin + route

/** A failed response, with its status. */
class HttpError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message)
  }
}

/** The JSON body, or an error with the server's reason (`{ error }`, or plain text from axum). */
const readJson = async <T>(res: Response, what: string): Promise<T> => {
  const text = await res.text()
  let body: unknown
  try {
    body = JSON.parse(text)
  } catch {
    body = undefined
  }
  if (!res.ok) {
    const reason =
      (body as { error?: string } | undefined)?.error ??
      (text || res.statusText)
    throw new HttpError(`Could not ${what}: ${reason}`, res.status)
  }
  return body as T
}

/** In the order `mirrord up init` shows them. */
export const useModes = () =>
  useQuery<ModeInfo[]>({
    staleTime: Infinity,
    queryKey: ['upModes'],
    queryFn: async () => {
      const res = await fetch(url(ALL_API_ROUTES.upModes))
      return (await readJson<{ modes: ModeInfo[] }>(res, 'load the modes'))
        .modes
    },
  })

export interface KubeContext {
  name: string
  namespace: string | null
}

interface ContextsResponse {
  contexts: KubeContext[]
  current: string | null
}

/** The context targets come from, like `mirrord up init`. `null` when there is none. */
export const useCurrentContext = () =>
  useQuery<KubeContext | null>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['kubeCurrentContext'],
    queryFn: async () => {
      const res = await fetch(url(ALL_API_ROUTES.contexts))
      const { contexts, current } = await readJson<ContextsResponse>(
        res,
        'read the kube contexts',
      )
      return (
        contexts.find((context) => context.name === current) ??
        (current === null ? null : { name: current, namespace: null })
      )
    },
  })

export const useNamespaces = () =>
  useQuery<string[]>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['kubeNamespaces'],
    queryFn: async () => {
      const res = await fetch(url(ALL_API_ROUTES.namespaces))
      return (
        await readJson<{ namespaces: string[] }>(res, 'list the namespaces')
      ).namespaces
    },
  })

/** What the target question of `mirrord up init` lists for `mode`. */
export const useWorkloads = (namespace: string | undefined, mode: ModeId) =>
  useQuery<string[]>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['upWorkloads', namespace, mode],
    queryFn: async () => {
      const res = await fetch(
        url(ALL_API_ROUTES.upWorkloads(namespace ?? '', mode)),
      )
      return (
        await readJson<{ workloads: string[] }>(res, 'list the workloads')
      ).workloads
    },
    enabled: namespace !== undefined,
  })

/** The ports offered to intercept. */
export const usePorts = (target: PickedWorkload | null) =>
  useQuery<number[]>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['upPorts', target?.namespace, target?.path],
    queryFn: async () => {
      const res = await fetch(
        url(
          ALL_API_ROUTES.upPorts(target?.namespace ?? '', target?.path ?? ''),
        ),
      )
      return (
        await readJson<{ ports: number[] }>(
          res,
          `detect the ports of ${target?.path}`,
        )
      ).ports
    },
    enabled: target !== null,
  })

/** The `mirrord-up.yaml`, rendered by the server. */
export const useRenderedYaml = (services: PickedService[]) =>
  useQuery<string>({
    queryKey: ['upRender', services],
    queryFn: async () => {
      const res = await fetch(url(ALL_API_ROUTES.upRender), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ services }),
      })
      return (await readJson<{ yaml: string }>(res, 'render the file')).yaml
    },
    enabled: services.length > 0,
    // A 4xx fails the same way every time; retry anything else once.
    retry: (failures, error) =>
      failures < 1 &&
      !(error instanceof HttpError && error.status < FIRST_SERVER_ERROR),
  })
