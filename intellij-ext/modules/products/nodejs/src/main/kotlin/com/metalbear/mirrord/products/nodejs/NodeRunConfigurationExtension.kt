package com.metalbear.mirrord.products.nodejs

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
        return RunConfigurationSettingsEditor()
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
            config.envs = config.envs + env;
        }

        return null
    }

    override fun getEditorTitle(): String? {
        return null
    }

    override fun isApplicableFor(profile: AbstractNodeTargetRunProfile): Boolean {
        return true
    }
}