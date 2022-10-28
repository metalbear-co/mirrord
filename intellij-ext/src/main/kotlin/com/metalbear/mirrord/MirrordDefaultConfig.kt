package com.metalbear.mirrord

import com.intellij.openapi.application.PathManager
import java.nio.file.Paths

data class MirrordDefaultConfig(
    val ldPreloadPath: String = getSharedLibPath("libmirrord_layer.so"),
    val dylibPath: String = getSharedLibPath("libmirrord_layer.dylib"),
    val acceptInvalidCertificates: Boolean = true,
    val skipProcesses: String = "",
    val fileOps: Boolean = true,
    val stealTraffic: Boolean = true,
    val telemetry: Boolean = true,
    val ephemeralContainers: Boolean = false,
    val remoteDns: Boolean = false,
    val tcpOutgoingTraffic: Boolean = false,
    val udpOutgoingTraffic: Boolean = false,
    val agentRustLog: LogLevel = LogLevel.INFO,
    val rustLog: LogLevel = LogLevel.INFO,
    val overrideEnvVarsExclude: String = "",
    val overrideEnvVarsInclude: String = "*",
    val ignorePorts: String = "",
)

private fun getSharedLibPath(libName: String): String {
    val path = Paths.get(PathManager.getPluginsPath(), "mirrord", libName).toString()

    if (System.getProperty("os.name").toLowerCase().contains("win")) {
        val wslRegex = "^[a-zA-Z]:".toRegex()

        val wslPath = wslRegex.replace(path) { drive ->
            "/mnt/" + drive.value.toLowerCase().removeSuffix(":")
        }

        return wslPath.replace("\\", "/")
    }

    return path
}
