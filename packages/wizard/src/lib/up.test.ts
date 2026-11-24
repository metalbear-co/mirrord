import { describe, it, expect } from 'vitest'
import {
  inNamespace,
  newDraft,
  nameError,
  parsePorts,
  toPickedService,
  type ServiceDraft,
} from './up'

const onEcho: ServiceDraft = {
  ...newDraft('split'),
  target: { path: 'deployment/server-echo', namespace: 'default' },
  name: 'server-echo',
  command: 'node server.js',
}

describe('parsePorts', () => {
  it('parses a comma-separated list, ignoring blanks', () => {
    expect(parsePorts(' 80, ,9090,')).toEqual({ ports: [80, 9090] })
    expect(parsePorts('+80')).toEqual({ ports: [80] })
    expect(parsePorts('')).toEqual({ ports: [] })
  })

  it('names the first entry that is not a port', () => {
    expect(parsePorts('80, http')).toEqual({
      error: '`http` is not a valid u16 port',
    })
    expect(parsePorts('70000')).toEqual({
      error: '`70000` is not a valid u16 port',
    })
    expect(parsePorts('-1')).toEqual({
      error: '`-1` is not a valid u16 port',
    })
  })
})

describe('nameError', () => {
  it('rejects blank and taken names', () => {
    expect(nameError('  ', [])).toBe('name cannot be empty')
    expect(nameError(' worker ', ['worker'])).toBe(
      'a service with that name already exists',
    )
    expect(nameError('worker', ['api'])).toBeNull()
  })
})

describe('inNamespace', () => {
  it('drops a workload from another namespace, but not a targetless service', () => {
    expect(inNamespace(onEcho, 'default')).toBe(onEcho)
    expect(inNamespace(onEcho, 'staging')).toMatchObject({
      target: null,
      name: '',
    })
    const targetless = { ...newDraft(), targetless: true, name: 'worker' }
    expect(inNamespace(targetless, 'staging')).toBe(targetless)
  })
})

describe('toPickedService', () => {
  it('keeps only the target and command when the traffic is not configured', () => {
    expect(
      toPickedService({
        ...onEcho,
        headerFilter: 'x-user: me',
        unselectedPorts: [80],
        otherIgnoredPorts: '1234',
      }),
    ).toEqual({
      name: 'server-echo',
      mode: 'split',
      target: { path: 'deployment/server-echo', namespace: 'default' },
      header_filter: null,
      ignore_ports: [],
      command: 'node server.js',
      directory: null,
    })
  })

  it('runs a targetless service without filters or ports', () => {
    const service = toPickedService({
      ...newDraft(),
      targetless: true,
      name: ' worker ',
      configureTraffic: true,
      headerFilter: 'x-user: me',
      otherIgnoredPorts: '80',
      command: 'python worker.py',
    })
    expect(service).toMatchObject({
      name: 'worker',
      target: null,
      header_filter: null,
      ignore_ports: [],
    })
  })
})
