Experimental Windows support: the layer is injected with stork and installs its
hooks before a launched program reaches its entry point. The injection method
can be chosen with `MIRRORD_INJECTION_METHOD` (`load-library`, `apc` or `iat`).
`mirrord attach` reports why the layer failed to initialize instead of waiting
out a timeout, the layer log file records `info` events without `MIRRORD_LOG`,
and crash reports explain fast-fail and heap-corruption exits.
