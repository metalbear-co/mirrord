package com.metalbear.mirrord.products.webstorm

import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.openapi.ui.panel.ComponentPanelBuilder
import com.intellij.util.ui.JBUI
import java.awt.BorderLayout
import javax.swing.BoxLayout
import javax.swing.JCheckBox
import javax.swing.JPanel

class RunConfigurationSettingsPanel(configuration: RunConfigurationBase<*>?) : JPanel() {
//    private val useDirenvCheckbox: JCheckBox
//    private val trustDirenvCheckbox: JCheckBox
//
//    init {
//        useDirenvCheckbox = JCheckBox("Enable Direnv")
//        trustDirenvCheckbox = JCheckBox("Trust .envrc")
//        val optionsPanel = JPanel()
//        val bl2 = BoxLayout(optionsPanel, BoxLayout.PAGE_AXIS)
//        optionsPanel.layout = bl2
//        optionsPanel.border = JBUI.Borders.emptyLeft(20)
//        optionsPanel.add(
//            ComponentPanelBuilder(trustDirenvCheckbox).withComment(
//                "When enabled it will automatically allow direnv to process changes to the .envrc file in " +
//                        "the working directory. Only enable this for projects you trust, direnv can execute " +
//                        "potentially malicious code."
//            ).createPanel()
//        )
//        val boxLayoutWrapper = JPanel()
//        val bl1 = BoxLayout(boxLayoutWrapper, BoxLayout.PAGE_AXIS)
//        boxLayoutWrapper.layout = bl1
//        boxLayoutWrapper.add(ComponentPanelBuilder(useDirenvCheckbox).createPanel())
//        boxLayoutWrapper.add(optionsPanel)
//        layout = BorderLayout()
//        add(boxLayoutWrapper, BorderLayout.NORTH)
//    }
}