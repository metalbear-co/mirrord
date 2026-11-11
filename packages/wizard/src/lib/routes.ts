// All api routes should be defined here, and then imported in the code elsewhere.
// Since we use `window.location.origin`, routes should start with a '/'.

// `/api/v1` routes are the wizard's own; the `up` ones serve the code of `mirrord up init`.
// `/api/v2/kube` is shared with the session monitor.
const ALL_API_ROUTES = {
  isReturning: '/api/v1/is-returning',
  upModes: '/api/v1/up/modes',
  upWorkloads: (namespace: string, mode: string) =>
    '/api/v1/up/workloads?' +
    new URLSearchParams({ namespace, mode }).toString(),
  upPorts: (namespace: string, path: string) =>
    '/api/v1/up/ports?' + new URLSearchParams({ namespace, path }).toString(),
  upRender: '/api/v1/up/render',
  contexts: '/api/v2/kube/contexts',
  namespaces: '/api/v2/kube/namespaces',
}

export default ALL_API_ROUTES
