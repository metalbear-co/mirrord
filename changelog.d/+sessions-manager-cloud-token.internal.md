Authenticate agents and intproxies to sessions-manager itself, by exchanging a
long-lived MetalBear API key for a short-lived token and sending it as
`Authorization: Bearer <token>` on control-plane requests. Set
`MIRRORD_SESSIONS_MANAGER_API_KEY` to enable it, and `MIRRORD_METALBEAR_CLOUD_URL`
to point the exchange at somewhere other than `https://app.metalbear.com`. For
local development, `MIRRORD_METALBEAR_CLOUD_BAGGAGE_SESSION` tags the exchange
with `baggage: mirrord-session=<key>` so it routes to a locally-run app-server
under mirrord, the same way the operator's `OPERATOR_CLOUD_BAGGAGE_SESSION` does.
This is independent of the `MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN` shared secret,
which keeps meaning "whatever fronts sessions-manager"; both may be set at once.
