Added `key` field to `MirrordOperator.status.sessions[]`, surfaced from the user's `key` value in the mirrord config. Lets clients group existing sessions by this key for discovery and reuse.
