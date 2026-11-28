import { useEffect, useSyncExternalStore } from 'react'
import { api } from './api'

// The namespaces of the kube context the user is looking at, for the namespace pickers. Every tab
// views the same context (`contextStore.ts`), so they share one list instead of each listing the
// namespaces again. Each tab still keeps its own selected namespace.

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

const LOADING: NamespaceList = { namespaces: [], loading: true, error: false }
const NO_CONTEXT: NamespaceList = {
  namespaces: [],
  loading: false,
  error: false,
}

/** The list of the context that was asked for last. */
let listed: { context: string; list: NamespaceList } | null = null

/** Increases with each listing, so that only the latest listing's response is kept. */
let generation = 0

const listeners = new Set<() => void>()

function publish(next: typeof listed): void {
  listed = next
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/**
 * Lists the namespaces of `context`, unless they are already listed or being listed for it. The
 * list is dropped whenever another context is asked for, so going back to a context lists it
 * again and shows the namespaces created since.
 */
function listNamespaces(context: string): void {
  if (listed?.context === context) return

  const request = ++generation
  publish({ context, list: LOADING })

  api
    .listNamespaces(context)
    .then(({ namespaces }) => {
      if (request !== generation) return
      publish({ context, list: { namespaces, loading: false, error: false } })
    })
    .catch((err: unknown) => {
      if (request !== generation) return
      console.error(err)
      publish({
        context,
        list: { namespaces: [], loading: false, error: true },
      })
    })
}

/**
 * The namespaces of `context`, for the namespace picker. While they load, this is an empty list
 * that is still loading, never the list of another context, so a picker can't offer a namespace
 * from the previous cluster.
 */
export function useNamespaces(context: string | null): NamespaceList {
  useEffect(() => {
    if (context) listNamespaces(context)
  }, [context])

  const current = useSyncExternalStore(subscribe, () => listed)

  if (!context) return NO_CONTEXT
  return current?.context === context ? current.list : LOADING
}
