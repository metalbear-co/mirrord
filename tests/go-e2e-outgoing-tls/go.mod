module go-e2e-outgoing-tls

// Kept at 1.25 on purpose: the `go` directive selects the GODEBUG compatibility
// defaults, so a module declaring an older version would not exercise the same
// runtime and crypto/tls behaviour that this app is here to cover.
go 1.25
