package com.metalbear.mirrord

import com.intellij.openapi.application.PathManager
import java.nio.file.Paths

data class MirrordDefaultData(val ldPreloadPath: String, val dylibPath: String, val agentLog: String, val rustLog: String, val acceptInvalidCertificates: Boolean, val ephemeralContainers: Boolean) {
    constructor() : this(getSharedLibPath("libmirrord_layer.so"), getSharedLibPath("libmirrord_layer.dylib"), "DEBUG", "DEBUG", true, false)
}

private fun getSharedLibPath(libName: String): String {
    val path = Paths.get(PathManager.getPluginsPath(), "mirrord", libName).toString()

    if (System.getProperty("os.name").toLowerCase().contains("win")) {
        val wslRegex = "^[a-zA-Z]:".toRegex()

         val wslPath = wslRegex.replace(path) {
                drive -> "/mnt/" + drive.value.toLowerCase().removeSuffix(":")
        }

        return wslPath.replace("\\", "/")
    }

    return path
}
