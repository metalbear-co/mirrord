The `mirrord ui` session monitor no longer shows an operator error when the operator's status
service is momentarily unavailable (e.g. during a pod restart). It now keeps the last-known team
sessions on screen and displays a "Reconnecting to operator…" hint instead.
