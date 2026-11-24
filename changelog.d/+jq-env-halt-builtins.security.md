User-supplied jq filters can no longer call `env`, `halt`, or `halt_error`. A filter that calls one of these now fails with an `Undefined filter` error.
