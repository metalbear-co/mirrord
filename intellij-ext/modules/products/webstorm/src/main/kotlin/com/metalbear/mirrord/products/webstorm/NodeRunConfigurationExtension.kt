package com.metalbear.mirrord.products.webstorm

import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.javascript.nodejs.execution.AbstractNodeTargetRunProfile
import com.intellij.javascript.nodejs.execution.runConfiguration.AbstractNodeRunConfigurationExtension
import com.intellij.javascript.nodejs.execution.runConfiguration.NodeRunConfigurationLaunchSession
import com.intellij.openapi.options.SettingsEditor
import com.jetbrains.nodejs.run.NodeJsRunConfiguration
import com.metalbear.mirrord.MirrordExecManager

class NodeRunConfigurationExtension: AbstractNodeRunConfigurationExtension() {

    override fun <P : AbstractNodeTargetRunProfile> createEditor(configuration: P): SettingsEditor<P> {
        return RunConfigurationSettingsEditor(configuration)
    }

    override fun createLaunchSession(
        configuration: AbstractNodeTargetRunProfile,
        environment: ExecutionEnvironment
    ): NodeRunConfigurationLaunchSession? {
        val project = configuration.project

        // Find out if we're running in wsl.
        val wsl = when (val request = createEnvironmentRequest(configuration, project)) {
            is WslTargetEnvironmentRequest -> request.configuration.distribution!!
            else -> null
        }


        MirrordExecManager.start(wsl, project)?.let {
                env ->
            val config = configuration as NodeJsRunConfiguration
            val unionList = (config.envs.asSequence() + env.asSequence())
                .distinct()
                .groupBy({ it.key }, { it.value })
                .mapValues { (_, values) -> values.joinToString(",") }
            config.envs = unionList
        }

        return null
    }

    override fun getEditorTitle(): String {
        return "Node Run Configuration"
    }

    override fun isApplicableFor(profile: AbstractNodeTargetRunProfile): Boolean {
        // TODO: do we sometimes want to return false here?
        return true
    }
}