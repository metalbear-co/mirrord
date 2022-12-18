package com.metalbear.mirrord

data class MirrordDefaultConfig(
    val acceptInvalidCertificates: Boolean = false,
    val skipProcesses: String = "",
    val fileOps: Boolean = true,
    val stealTraffic: Boolean = false,
    val telemetry: Boolean = true,
    val ephemeralContainers: Boolean = false,
    val remoteDns: Boolean = true,
    val tcpOutgoingTraffic: Boolean = true,
    val udpOutgoingTraffic: Boolean = true,
    val agentRustLog: LogLevel = LogLevel.WARN,
    val rustLog: LogLevel = LogLevel.WARN,
    val overrideEnvVarsExclude: String = "",
    val overrideEnvVarsInclude: String = "*",
    // ignorePorts: temporary patch to ignore ports which would cause `connect` to fail
    // as the debugger tried to connect to higher ranged ports.
    val ignorePorts: String = "45000-65535",
)
