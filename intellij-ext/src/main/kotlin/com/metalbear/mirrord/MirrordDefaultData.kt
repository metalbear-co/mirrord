package com.metalbear.mirrord

import com.intellij.openapi.application.PathManager
import java.nio.file.Paths

data class MirrordDefaultData(val ldPreloadPath: String, val dylibPath: String, val agentLog: String, val rustLog: String, val acceptInvalidCertificates: Boolean, val ephemeralContainers: Boolean) {
    constructor() : this(getSharedLibPath("libmirrord_layer.so"), getSharedLibPath("libmirrord_layer.dylib"), "DEBUG", "DEBUG", true, false)
}

private fun getSharedLibPath(libName: String): String {
    return Paths.get(PathManager.getPluginsPath(), "mirrord", libName).toString()
}
