package com.metalbear.mirrord.products.webstorm

import com.intellij.execution.Executor
//import com.intellij.execution.RunConfigurationExtension
//import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.javascript.nodejs.execution.runConfiguration.AbstractNodeRunConfigurationExtension;
import com.intellij.openapi.externalSystem.service.execution.ExternalSystemRunConfiguration
import com.metalbear.mirrord.MirrordExecManager
import com.metalbear.mirrord.MirrordLogger
import com.jetbrains.nodejs.run.NodeJsRunConfiguration
import com.jetbrains.nodejs.run.NodeJSRunConfigurationExtension

import java.util.concurrent.ExecutionException

class NodeRunConfigurationExtension: NodeJsRunConfigurationExtension() {
    override fun isApplicableFor(configuration: AbstractNodeRunConfigurationExtension<*>): Boolean {
        return true
    }

    override fun isEnabledFor(
        applicableConfiguration: AbstractNodeRunConfigurationExtension<*>,
        runnerSettings: RunnerSettings?
    ): Boolean {
        return true
    }


    override fun patchCommandLine(
        configuration: AbstractNodeRunConfigurationExtension<*>,
        runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine,
        runnerId: String
    ) {
        val wsl = when (val request = createEnvironmentRequest(configuration, configuration.project)) {
            is WslTargetEnvironmentRequest -> request.configuration.distribution!!
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