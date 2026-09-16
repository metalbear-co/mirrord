Send an optional `x-mirrord-sm-auth` shared-secret header on sessions-manager
control-plane and data-plane connections, so a deployment can put an
authenticating proxy in front of sessions-manager. Set
`MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN` to enable it.
