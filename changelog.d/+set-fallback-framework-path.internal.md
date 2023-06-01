On macOS, if we path a binary for SIP and it is in a path that is inside a directory that has a name that ends with `.app`, we add the frameworks directory to `DYLD_FALLBACK_FRAMEWORK_PATH`.
