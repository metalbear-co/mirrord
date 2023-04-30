package com.metalbear.mirrord.products.nodejs

import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.options.SettingsEditor
import javax.swing.JComponent
import javax.swing.JPanel

class RunConfigurationSettingsEditor<T : RunConfigurationBase<*>>(configuration: RunConfigurationBase<*>?) :
    SettingsEditor<T>() {
    private val editor: JPanel = JPanel()

    override fun resetEditorFrom(configuration: T) {
        // TODO: do we have to do something here?
    }

    @Throws(ConfigurationException::class)
    override fun applyEditorTo(configuration: T) {
        // TODO: do we have to do something here?
    }

    override fun createEditor(): JComponent {
        return editor
    }
}