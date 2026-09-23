Ask sessions-manager credential providers for control-plane and data-plane
headers separately and asynchronously, so a provider can fetch credentials over
the network and keep a control-plane-only credential off the data-plane upgrade.
The `MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN` shared secret is still sent on both
planes.
