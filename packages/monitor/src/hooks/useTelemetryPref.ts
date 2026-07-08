import { useCallback, useEffect, useState } from 'react'

const STORAGE_KEY = 'session-monitor-telemetry'

function readStored(): boolean {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw === 'off') return false
    return true
  } catch {
    return true
  }
}

/**
 * User-controlled toggle for sending anonymous usage analytics from the session monitor UI.
 * Defaults to enabled; the session's own `config.telemetry = false` still overrides this to
 * off in App.tsx — this preference only lets the user opt out further, not override a
 * session's opt-out.
 */
export function useTelemetryPref(): [boolean, (next: boolean) => void] {
  const [enabled, setEnabled] = useState<boolean>(readStored)

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, enabled ? 'on' : 'off')
    } catch {
      // localStorage can fail in private browsing; preference is per-tab only in that case.
    }
  }, [enabled])

  const set = useCallback((next: boolean) => {
    setEnabled(next)
  }, [])

  return [enabled, set]
}
