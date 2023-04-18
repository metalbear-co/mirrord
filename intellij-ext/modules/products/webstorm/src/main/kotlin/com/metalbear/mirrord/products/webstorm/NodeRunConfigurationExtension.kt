package com.metalbear.mirrord.products.webstorm

//import com.intellij.execution.RunConfigurationExtension
//import com.intellij.execution.configurations.JavaParameters

// Mensioned
//import com.jetbrains.nodejs.run.NodeJSRunConfigurationExtension

import com.intellij.javascript.nodejs.execution.AbstractNodeTargetRunProfile
import com.intellij.javascript.nodejs.execution.runConfiguration.AbstractNodeRunConfigurationExtension
import com.intellij.openapi.options.SettingsEditor

// I think this is not what we want, because we don't want to change anything about the configuration editor or manage
// it ourselves.
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