package com.metalbear.mirrord

import com.intellij.ide.BrowserUtil
import com.intellij.ui.components.ActionLink
import com.intellij.ui.components.JBCheckBox
import com.intellij.ui.components.labels.LinkLabel
import com.intellij.util.ui.FormBuilder
import javax.swing.JComponent
import javax.swing.JPanel


class MirrordSettingsComponent {

    private val telemetryEnabled = JBCheckBox("Telemetry")
    val panel: JPanel
    init {
        val externalLink = ActionLink("Read more") { _ -> BrowserUtil.browse("https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md") }
        panel = FormBuilder.createFormBuilder().setAlignLabelOnRight(true)
            .addLabeledComponent(telemetryEnabled, externalLink)
            .addComponentFillVertically(JPanel(), 0)
            .panel
    }


    val preferredFocusedComponent: JComponent
        get() = telemetryEnabled

    var telemetryEnabledStatus: Boolean
        get() = telemetryEnabled.isSelected
        set(newStatus) {
            telemetryEnabled.isSelected = newStatus
        }
}