// All api routes should be defined here, and then imported in the code elsewhere.
// Since we use `window.location.origin`, routes should start with a '/'.
// Note that since we have a "catch-all" path in `../App.tsx`, fetching from non-existent api paths
// can behave unexpectedly.

const ALL_API_ROUTES = {
  isReturning: "/api/v1/is-returning",
  clusterDetails: "/api/v1/cluster-details",
  targets: (namespace: string, targetType?: string) =>
    "/api/v1/namespace/" +
    namespace +
    "/targets" +
    (targetType ? "?target_type=" + targetType : ""),
};

export default ALL_API_ROUTES;
