import { useCallback, useEffect, useState } from 'react'
import { TELEMETRY_STORAGE_KEY, readTelemetryPref } from '../analytics'

/**
 * User-controlled toggle for sending anonymous usage analytics from the session monitor UI.
 * Defaults to enabled; the session's own `config.telemetry = false` still overrides this to
 * off in App.tsx — this preference only lets the user opt out further, not override a
 * session's opt-out.
 */
export function useTelemetryPref(): [boolean, (next: boolean) => void] {
  const [enabled, setEnabled] = useState<boolean>(readTelemetryPref)

  useEffect(() => {
    try {
      localStorage.setItem(TELEMETRY_STORAGE_KEY, enabled ? 'on' : 'off')
    } catch {
      // localStorage can fail in private browsing; preference is per-tab only in that case.
    }
  }, [enabled])

  const set = useCallback((next: boolean) => {
    setEnabled(next)
  }, [])

  return [enabled, set]
}
