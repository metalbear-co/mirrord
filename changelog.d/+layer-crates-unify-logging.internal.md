Migrates the MIRRORD_LAYER_LOG_PATH capability to layer-lib, making it also accessible to the unix layer crate.

MIRRORD_LAYER_LOG_PATH allows for the specification of a directory in which layer tracing logs will be written, one per process in the format `mirrord-layer_{%Y%m%d_%H%M%S}_{pid}_{processName}`.
Where processName is a sanitized version which only contains characters from alphanumeric, `-`, `_`; any other character falls back to `_`.