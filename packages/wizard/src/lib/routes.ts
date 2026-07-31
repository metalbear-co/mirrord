// All api routes should be defined here, and then imported in the code elsewhere.
// Since we use `window.location.origin`, routes should start with a '/'.

// The wizard's cluster reads share the session monitor's context/namespace-aware `/api/v2/kube`
// API. `is-returning` stays on the wizard's own `/api/v1` (it's wizard onboarding state, not kube).
const ALL_API_ROUTES = {
  isReturning: '/api/v1/is-returning',
  contexts: '/api/v2/kube/contexts',
  namespaces: (context?: string) =>
    '/api/v2/kube/namespaces' +
    (context ? '?context=' + encodeURIComponent(context) : ''),
  targetTypes: '/api/v2/kube/target-types',
  targets: (namespace: string, targetType?: string, context?: string) => {
    const params = new URLSearchParams()
    params.set('namespace', namespace)
    if (targetType) params.set('target_type', targetType)
    if (context) params.set('context', context)
    return '/api/v2/kube/targets?' + params.toString()
  },
}

export default ALL_API_ROUTES
