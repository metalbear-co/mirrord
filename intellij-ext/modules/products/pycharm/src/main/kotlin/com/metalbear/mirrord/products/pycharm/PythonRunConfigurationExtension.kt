package com.metalbear.mirrord.products.pycharm

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.wsl.WslPath.Companion.getDistributionByWindowsUncPath
import com.intellij.openapi.util.SystemInfo
import com.jetbrains.python.run.AbstractPythonRunConfiguration
import com.jetbrains.python.run.PythonRunConfigurationExtension
import com.metalbear.mirrord.MirrordExecManager


class PythonRunConfigurationExtension: PythonRunConfigurationExtension() {
    override fun isApplicableFor(configuration: AbstractPythonRunConfiguration<*>): Boolean {
        return true
    }

    override fun isEnabledFor(
        applicableConfiguration: AbstractPythonRunConfiguration<*>,
        runnerSettings: RunnerSettings?
    ): Boolean {
        return true
    }


    override fun patchCommandLine(
        configuration: AbstractPythonRunConfiguration<*>,
        runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine,
        runnerId: String
    ) {
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
                currentEnv[entry.key] =  entry.value
            }
        }
    }

}