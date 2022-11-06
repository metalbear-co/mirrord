package com.metalbear.mirrord

import com.intellij.openapi.application.PathManager
import java.nio.file.Paths

data class MirrordDefaultConfig(
    val ldPreloadPath: String = getSharedLibPath("libmirrord_layer.so"),
    val dylibPath: String = getSharedLibPath("libmirrord_layer.dylib"),
    val acceptInvalidCertificates: Boolean = false,
    val skipProcesses: String = "",
    val fileOps: Boolean = true,
    val stealTraffic: Boolean = false,
    val telemetry: Boolean = true,
    val ephemeralContainers: Boolean = false,
    val remoteDns: Boolean = true,
    val tcpOutgoingTraffic: Boolean = true,
    val udpOutgoingTraffic: Boolean = true,
    val agentRustLog: LogLevel = LogLevel.INFO,
    val rustLog: LogLevel = LogLevel.INFO,
    val overrideEnvVarsExclude: String = "",
    val overrideEnvVarsInclude: String = "*",
    // ignorePorts: temporary patch to ignore ports which would cause `connect` to fail
    // as the debugger tried to connect to higher ranged ports.
    val ignorePorts: String = "45000-65535",
    val defaultPodNamespace: String = "default"
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
