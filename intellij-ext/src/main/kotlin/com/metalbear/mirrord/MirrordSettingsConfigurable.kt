package com.metalbear.mirrord

import com.intellij.openapi.options.Configurable
import javax.swing.JComponent


class MirrordSettingsConfigurable : Configurable {
    private var mySettingsComponent: MirrordSettingsComponent? = null

    override fun getDisplayName(): String {
        return "mirrord"
    }
    override fun getPreferredFocusedComponent(): JComponent {
        return mySettingsComponent!!.preferredFocusedComponent
    }

    override fun createComponent(): JComponent {
        mySettingsComponent = MirrordSettingsComponent()
        return mySettingsComponent!!.panel
    }

    override fun isModified(): Boolean
        {
            val settings = MirrordSettingsState
            return (mySettingsComponent!!.telemetryEnabledStatus != settings.telemetryEnabled)
        }

    override fun apply() {
        val settings = MirrordSettingsState
        settings.telemetryEnabled = mySettingsComponent!!.telemetryEnabledStatus
    }

    override fun reset() {
        val settings = MirrordSettingsState
        mySettingsComponent!!.telemetryEnabledStatus = settings.telemetryEnabled ?: false
    }

    override fun disposeUIResources() {
        mySettingsComponent = null
    }
}