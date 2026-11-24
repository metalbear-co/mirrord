import { useEffect, useState } from 'react'
import { api } from '../api'

export interface NamespaceList {
  namespaces: string[]
  loading: boolean
  /**
   * Set when listing namespaces fails, such as when an RBAC policy denies `list namespaces`, which
   * is common on strict multi-tenant clusters. The picker then lets the user type a namespace
   * instead.
   */
  error: boolean
}

/**
 * The namespaces of `context`, for the namespace picker, listed again whenever the context changes.
 * A list that arrives after the context changed again is discarded.
 */
export function useNamespaces(context: string | null): NamespaceList {
  const [namespaces, setNamespaces] = useState<string[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState(false)

  useEffect(() => {
    if (!context) return
    let cancelled = false
    setLoading(true)
    api
      .listNamespaces(context)
      .then(({ namespaces: nextNamespaces }) => {
        if (cancelled) return
        setNamespaces(nextNamespaces)
        setError(false)
      })
      .catch((err: unknown) => {
        if (cancelled) return
        console.error(err)
        setNamespaces([])
        setError(true)
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [context])

  return { namespaces, loading, error }
}
