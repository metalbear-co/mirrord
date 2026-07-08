// All api routes should be defined here, and then imported in the code elsewhere.
// Since we use `window.location.origin`, routes should start with a '/'.

const ALL_API_ROUTES = {
  // Served by the shared `mirrord ui` server for the monitor; the wizard reuses it for its
  // kube context picker.
  contexts: '/api/contexts',
  clusterDetails: (context?: string) =>
    '/api/v1/cluster-details' +
    (context ? '?context=' + encodeURIComponent(context) : ''),
  targets: (namespace: string, targetType?: string, context?: string) => {
    const params = new URLSearchParams()
    if (targetType) params.set('target_type', targetType)
    if (context) params.set('context', context)
    const query = params.toString()
    return (
      '/api/v1/namespace/' + namespace + '/targets' + (query ? '?' + query : '')
    )
  },
}

export default ALL_API_ROUTES
