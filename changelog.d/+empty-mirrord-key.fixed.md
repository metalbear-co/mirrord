An empty `MIRRORD_KEY` now counts as unset, so the `key` from the config file is used instead of an empty key that made `{{ key }}` filters match every session.
