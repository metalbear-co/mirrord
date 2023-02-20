package com.metalbear.mirrord.products.idea

import com.intellij.execution.Executor
import com.intellij.execution.RunConfigurationExtension
import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.target.getEffectiveTargetName
import com.intellij.execution.wsl.WslPath.Companion.getDistributionByWindowsUncPath
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.util.SystemInfo
import com.metalbear.mirrord.MirrordExecManager
import com.metalbear.mirrord.MirrordLogger

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
        MirrordLogger.logger.debug("wsl check")
        val wsl = when (val request = createEnvironmentRequest(configuration, configuration.project)) {
            is WslTargetEnvironmentRequest -> request.configuration.distribution!!
            else -> null
        }

        MirrordLogger.logger.debug("getting env")
        val project = configuration.project
        val currentEnv = HashMap<String, String>()
        currentEnv.putAll(params.env)

        MirrordLogger.logger.debug("calling start")
        MirrordExecManager.start(wsl, project)?.let {
                env ->
            for (entry in env.entries.iterator()) {
                currentEnv[entry.key] =  entry.value
            }
        }
        params.env = currentEnv
        MirrordLogger.logger.debug("setting env and finishing")
    }
    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
        configuration: T,
        params: JavaParameters,
        runnerSettings: RunnerSettings?,
        executor: Executor
    ) {
        MirrordLogger.logger.debug("updateJavaParameters called")
        patchEnv(configuration, params)
    }

    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
        configuration: T,
        params: JavaParameters,
        runnerSettings: RunnerSettings?
    ) {
        MirrordLogger.logger.debug("updateJavaParameters (with less parameters) called")
        patchEnv(configuration, params)
    }
}