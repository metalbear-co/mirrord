package com.metalbear.mirrord.products.idea

import com.intellij.execution.RunConfigurationExtension
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.wsl.WslPath.Companion.getDistributionByWindowsUncPath
import com.intellij.openapi.util.SystemInfo
import com.metalbear.mirrord.MirrordExecManager
import org.jetbrains.annotations.NotNull

class IdeaRunConfigurationExtension: RunConfigurationExtension() {
    override fun isApplicableFor(configuration: RunConfigurationBase<*>): Boolean {
        return true
    }


    override fun patchCommandLine(
        configuration: RunConfigurationBase<*>,
        runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine,
        runnerId: String
    ) {

        // dunno if this works
        val wsl = when {
            SystemInfo.isWindows -> {
                getDistributionByWindowsUncPath(configuration.projectPathOnTarget)
            }
            else -> null
        }

        val project = configuration.project
        val currentEnv = cmdLine.environment

        MirrordExecManager.start(wsl, project)?.let {
                env ->
            for (entry in env.entries.iterator()) {
                currentEnv.put(entry.key, entry.value)
            }
        }

        super.patchCommandLine(configuration, runnerSettings, cmdLine, runnerId)
    }


    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
        configuration: T,
        params: JavaParameters,
        runnerSettings: RunnerSettings?
    ) {
        return
    }
}