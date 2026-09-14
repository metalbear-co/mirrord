Authenticate agents and intproxies to sessions-manager itself, by exchanging a
long-lived MetalBear API key for a short-lived token and sending it as
`Authorization: Bearer <token>` on control-plane requests. Set
`MIRRORD_SESSIONS_MANAGER_API_KEY` to enable it, and `MIRRORD_METALBEAR_CLOUD_URL`
to point the exchange at somewhere other than `https://app.metalbear.com`. A token
whose `scope` is not `serverless_agent` is logged as a warning but still used;
sessions-manager is the one that rejects it. This is independent of the
`MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN` shared secret, which keeps meaning
"whatever fronts sessions-manager"; both may be set at once.
