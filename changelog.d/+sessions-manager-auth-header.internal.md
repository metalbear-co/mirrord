Send an optional shared-secret header on sessions-manager control-plane and
data-plane connections, so a deployment can put an authenticating proxy in front
of sessions-manager. Set `MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN` to enable it, and
`MIRRORD_SESSIONS_MANAGER_AUTH_HEADER` to override the default `x-mirrord-sm-auth`
header name.
