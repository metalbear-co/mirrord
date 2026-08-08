import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import ErrorBoundary from './ErrorBoundary'
import { emitWizardBlocked } from '../analytics'

vi.mock('../analytics', () => ({
  emitWizardBlocked: vi.fn(),
}))

function Thrower(): never {
  throw new Error('component exploded')
}

describe('ErrorBoundary', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.spyOn(console, 'error').mockImplementation(() => undefined)
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('reports the crash to telemetry and renders the fallback', () => {
    render(
      <ErrorBoundary>
        <Thrower />
      </ErrorBoundary>,
    )

    expect(screen.getByText('component exploded')).toBeInTheDocument()
    expect(emitWizardBlocked).toHaveBeenCalledWith(
      'ui_crashed',
      'user_action',
      expect.objectContaining({ error: 'component exploded' }),
      expect.any(Error),
    )
  })
})
