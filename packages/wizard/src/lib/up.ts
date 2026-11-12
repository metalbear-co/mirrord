// What the user picks for each service, and helpers to build it from the service being edited.
// The wizard asks the questions of `mirrord up init` (`mirrord/up/src/init.rs`); the server renders
// the picks with the same code (`mirrord_up::render_skeleton`).

export type ModeId = 'split' | 'mirror' | 'replace'

/** From `GET /api/v1/up/modes`. */
export interface ModeInfo {
  id: ModeId
  label: string
  description: string
  recommended: boolean
  /** Kinds the target question lists, like `deployment`. */
  target_types: string[]
  /** Offers to run without a target. */
  targetless: boolean
  /** Asks for an HTTP header filter. */
  http_filter: boolean
  /** The yes/no question before the traffic ones. */
  traffic_question: string
}

export interface PickedWorkload {
  /** Like `deployment/api`. */
  path: string
  namespace: string
}

/** One service, as `POST /api/v1/up/render` takes it. */
export interface PickedService {
  name: string
  mode: ModeId
  /** `null` runs targetless. */
  target: PickedWorkload | null
  header_filter: string | null
  ignore_ports: number[]
  command: string
  directory: string | null
}

/** The service being edited. */
export interface ServiceDraft {
  mode: ModeId
  /** `null` until picked, or when targetless. */
  target: PickedWorkload | null
  targetless: boolean
  /** From the target, or typed in when targetless. */
  name: string
  /** The answer to the mode's `traffic_question`. */
  configureTraffic: boolean
  headerFilter: string
  /** Detected ports the user unticked, which are ignored. All start ticked. */
  unselectedPorts: number[]
  /** More ports to ignore, comma-separated. */
  otherIgnoredPorts: string
  command: string
  directory: string
}

const MAX_PORT = 65535

export const newDraft = (mode: ModeId = 'split'): ServiceDraft => ({
  mode,
  target: null,
  targetless: false,
  name: '',
  configureTraffic: false,
  headerFilter: '',
  unselectedPorts: [],
  otherIgnoredPorts: '',
  command: '',
  directory: '',
})

/**
 * Parses comma-separated ports like `mirrord up init` (Rust's `u16` parsing allows a leading `+`),
 * skipping blanks. The error names the first bad entry.
 */
export const parsePorts = (
  text: string,
): { ports: number[] } | { error: string } => {
  const ports: number[] = []
  for (const entry of text.split(',')) {
    const trimmed = entry.trim()
    if (trimmed === '') continue
    const port = Number(trimmed)
    if (!/^\+?\d+$/.test(trimmed) || port > MAX_PORT) {
      return { error: `\`${trimmed}\` is not a valid u16 port` }
    }
    ports.push(port)
  }
  return { ports }
}

/** The workload name, or `<workload>-2`, `-3`, ... when taken, like `mirrord up init`. */
export const serviceNameFor = (workload: string, taken: string[]): string => {
  let name = workload
  for (let suffix = 2; taken.includes(name); suffix++) {
    name = `${workload}-${suffix}`
  }
  return name
}

/** `api` for `deployment/api` or `pod/api/container/app`. */
export const workloadName = (path: string): string => path.split('/')[1] ?? path

/** `deployment` for `deployment/api`. */
export const workloadKind = (path: string): string => path.split('/')[0] ?? ''

/** `api/container/app` for `pod/api/container/app`. */
export const withoutKind = (path: string): string =>
  path.slice(path.indexOf('/') + 1)

/** The draft after switching to `namespace`: a workload picked in another one is dropped. */
export const inNamespace = (
  draft: ServiceDraft,
  namespace: string,
): ServiceDraft =>
  draft.target === null || draft.target.namespace === namespace
    ? draft
    : { ...draft, target: null, name: '', unselectedPorts: [] }

/** Why the name cannot be used, or `null`. */
export const nameError = (name: string, taken: string[]): string | null => {
  const trimmed = name.trim()
  if (trimmed === '') return 'name cannot be empty'
  if (taken.includes(trimmed)) return 'a service with that name already exists'
  return null
}

/** A target is picked, or targetless with a usable name. */
export const targetDone = (draft: ServiceDraft, taken: string[]): boolean =>
  draft.target !== null ||
  (draft.targetless && nameError(draft.name, taken) === null)

/** Why the traffic picks cannot be used, or `null`. */
export const trafficError = (draft: ServiceDraft): string | null => {
  if (!draft.configureTraffic) return null
  const parsed = parsePorts(draft.otherIgnoredPorts)
  return 'error' in parsed ? parsed.error : null
}

/** Why the command cannot be used, or `null`. */
export const commandError = (command: string): string | null =>
  command.trim() === '' ? 'command cannot be empty' : null

/** The picks of a finished draft. */
export const toPickedService = (draft: ServiceDraft): PickedService => {
  const traffic = draft.configureTraffic && draft.target !== null
  const parsed = parsePorts(draft.otherIgnoredPorts)
  const otherIgnored = 'ports' in parsed ? parsed.ports : []
  const ignorePorts = traffic ? [...draft.unselectedPorts, ...otherIgnored] : []
  const headerFilter = draft.headerFilter.trim()
  const directory = draft.directory.trim()

  return {
    name: draft.name.trim(),
    mode: draft.mode,
    target: draft.targetless ? null : draft.target,
    header_filter: traffic && headerFilter !== '' ? headerFilter : null,
    ignore_ports: [...new Set(ignorePorts)].sort((a, b) => a - b),
    command: draft.command,
    directory: directory === '' ? null : directory,
  }
}
