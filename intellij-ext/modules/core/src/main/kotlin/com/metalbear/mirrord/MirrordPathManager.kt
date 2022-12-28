package com.metalbear.mirrord

import com.intellij.openapi.application.PathManager
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.Files
import com.intellij.openapi.util.SystemInfo
import com.intellij.util.system.CpuArch

object MirrordPathManager {
    private fun pluginDir(): Path = Paths.get(PathManager.getPluginsPath(), "mirrord")

    /**
     * Get matching binary based on platform and architecture.
     */
    fun getBinary(name: String, universalOnMac: Boolean): String? {
            val os = when {
            SystemInfo.isLinux -> "linux"
            SystemInfo.isMac -> "macos"
            SystemInfo.isWindows -> "linux"
            else -> return null
        }

        val arch = when {
            CpuArch.isIntel64() -> "x86-64"
            CpuArch.isArm64() -> "arm64"
            else -> return null
        }

        val format = when {
            SystemInfo.isMac && universalOnMac -> "bin/$os/$name"
            else -> "bin/$os/$arch/$name"
        }

        val binaryPath = pluginDir().resolve(format).takeIf { Files.exists(it) } ?: return null
        return if (Files.isExecutable(binaryPath) || binaryPath.toFile().setExecutable(true)) {
            return binaryPath.toString()
        } else {
            null
        }


    }
}