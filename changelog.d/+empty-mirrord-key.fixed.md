An empty `MIRRORD_KEY`, or an empty `key` in the config file, now counts as unset, instead of becoming an empty key that made `{{ key }}` filters match every session.
