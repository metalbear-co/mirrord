import { describe, expect, it, vi } from 'vitest'

const init = vi.fn()

vi.mock('posthog-js', () => ({
  default: {
    init,
    capture: vi.fn(),
    captureException: vi.fn(),
    setPersonProperties: vi.fn(),
    group: vi.fn(),
    opt_in_capturing: vi.fn(),
    opt_out_capturing: vi.fn(),
    startSessionRecording: vi.fn(),
    stopSessionRecording: vi.fn(),
  },
}))

describe('analytics', () => {
  it('stamps the mirrord version on every event it sends', async () => {
    const { initAnalytics } = await import('./analytics')
    initAnalytics(true)

    const beforeSend = init.mock.calls[0][1].before_send
    const stamped = beforeSend({
      event: 'monitor_user_blocked',
      properties: { reason: 'ui_crashed' },
    })

    expect(stamped.properties.reason).toBe('ui_crashed')
    expect(stamped.properties.mirrord_version).toBeDefined()
  })
})
