package com.metalbear.mirrord.products.webstorm

import com.intellij.execution.RunConfigurationExtension
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
//import com.intellij.execution.configurations.JavaParameters

// Mensioned
//import com.jetbrains.nodejs.run.NodeJSRunConfigurationExtension

import com.intellij.javascript.nodejs.execution.AbstractNodeTargetRunProfile
import com.intellij.javascript.nodejs.execution.runConfiguration.AbstractNodeRunConfigurationExtension
import com.intellij.openapi.options.SettingsEditor
import com.metalbear.mirrord.MirrordExecManager

// This doesn't work, has to be of type AbstractNodeRunConfigurationExtension.
//class NodeRunConfigurationExtension: RunConfigurationExtension() {
//    override fun isApplicableFor(configuration: RunConfigurationBase<*>): Boolean {
////        return configuration.type.id == "node"
//        return true
//    }
//
//    override fun <T : RunConfigurationBase<*>> updateJavaParameters(p0: T, p1: JavaParameters, p2: RunnerSettings?) {
//        return
//    }
//
//    override fun patchCommandLine(
//        configuration: RunConfigurationBase<*>,
//        runnerSettings: RunnerSettings?,
//        cmdLine: GeneralCommandLine,
//        runnerId: String
//    ) {
//        val wsl = when (val request = createEnvironmentRequest(configuration, configuration.project)) {
//            is WslTargetEnvironmentRequest -> request.configuration.distribution!!
//            else -> null
//        }
//
//        val project = configuration.project
//        val currentEnv = cmdLine.environment
//
//        MirrordExecManager.start(wsl, project)?.let {
//                env ->
//            for (entry in env.entries.iterator()) {
//                currentEnv[entry.key] =  entry.value
//            }
//        }
//    }
//
//}

class NodeRunConfigurationExtension: AbstractNodeRunConfigurationExtension() {

    override fun <P : AbstractNodeTargetRunProfile> createEditor(configuration: P): SettingsEditor<P> {
        return RunConfigurationSettingsEditor(configuration)
    }

    override fun getEditorTitle(): String? {
        // TODO:
        return "Node Run Configuration"
    }

    override fun isApplicableFor(profile: AbstractNodeTargetRunProfile): Boolean {
        return true
    }
}