Requests for chaos rules with a latency effect now specify `"read_ms"` and `"write_ms"` instead of
`"delay_ms"`. They are applied in the read and write directions respectively and cannot both be 0.
