package com.metalbear.mirrord.products.idea

import com.intellij.execution.Executor
import com.intellij.execution.RunConfigurationExtension
import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.wsl.WslPath.Companion.getDistributionByWindowsUncPath
import com.intellij.openapi.util.SystemInfo
import com.metalbear.mirrord.MirrordExecManager

class IdeaRunConfigurationExtension: RunConfigurationExtension() {
    override fun isApplicableFor(configuration: RunConfigurationBase<*>): Boolean {
        return true
    }


    override fun isEnabledFor(
        applicableConfiguration: RunConfigurationBase<*>,
        runnerSettings: RunnerSettings?
    ): Boolean {
        return true
    }


    private fun < T: RunConfigurationBase<*>> patchEnv (configuration: T, params: JavaParameters) {
        val wsl = when {
            SystemInfo.isWindows -> {
                getDistributionByWindowsUncPath(configuration.projectPathOnTarget)
            }
            else -> null
        }


        val project = configuration.project
        val currentEnv = HashMap<String, String>()
        currentEnv.putAll(params.env)
        
        MirrordExecManager.start(wsl, project)?.let {
                env ->
            for (entry in env.entries.iterator()) {
                currentEnv[entry.key] =  entry.value
            }
        }
        params.env = currentEnv
    }
    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
        configuration: T,
        params: JavaParameters,
        runnerSettings: RunnerSettings?,
        executor: Executor
    ) {
        patchEnv(configuration, params)
    }

    override fun <T : RunConfigurationBase<*>?> updateJavaParameters(
        configuration: T,
        params: JavaParameters,
        runnerSettings: RunnerSettings?
    ) {
        patchEnv(configuration!!, params)
    }
}