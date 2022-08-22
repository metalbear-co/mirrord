package com.github.metalbear.intellijplugin

data class MirrordDefaultData(val ldPreloadPath: String, val dylibPath: String, val agentLog: String, val rustLog: String, val acceptInvalidCertificates: Boolean, val ephemeralContainers: Boolean) {
    constructor() : this("target/debug/libmirrord_layer.so", "target/debug/libmirrord_layer.dylib", "DEBUG", "DEBUG", true, false)
}

