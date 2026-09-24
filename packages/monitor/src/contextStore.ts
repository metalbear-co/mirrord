import { useEffect, useSyncExternalStore } from 'react'
import { api } from './api'
import type { KubeContext } from './types'

// The kube context the user is looking at, shared by every tab of the mirrord UI. Nothing here is
// persisted, so browser tabs still view clusters independently.

export interface ContextSelection {
  contexts: KubeContext[]
  /** The kubeconfig's current context. */
  current: string | null
  /** `null` means "follow `current`". */
  selected: string | null
}

let selection: ContextSelection = {
  contexts: [],
  current: null,
  selected: null,
}

let loaded = false

const listeners = new Set<() => void>()

function publish(next: ContextSelection): void {
  selection = next
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function selectKubeContext(context: string | null): void {
  publish({ ...selection, selected: context })
}

export function useContextSelection(): ContextSelection {
  return useSyncExternalStore(subscribe, () => selection)
}

/** Loads the kubeconfig's contexts once per page load, whichever tab mounts first. */
export function useKubeContexts(): ContextSelection {
  useEffect(() => {
    if (loaded) return
    loaded = true
    api
      .listContexts()
      .then(({ current, contexts }) =>
        publish({ ...selection, contexts, current }),
      )
      .catch((err: unknown) => {
        loaded = false
        console.error(err)
      })
  }, [])

  return useContextSelection()
}
