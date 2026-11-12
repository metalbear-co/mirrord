import { describe, it, expect } from 'vitest'
import { strings } from './strings'
import { MODES } from './test/fake-server'

// The wizard generates a `mirrord-up.yaml`, where steal mode is called split.
describe('strings', () => {
  const all = JSON.stringify(strings) + JSON.stringify(MODES)

  it('do not mention mirrord.json', () => {
    expect(all).not.toMatch(/mirrord\.json/i)
  })

  it('do not mention steal mode', () => {
    expect(all).not.toMatch(/steal/i)
  })
})
