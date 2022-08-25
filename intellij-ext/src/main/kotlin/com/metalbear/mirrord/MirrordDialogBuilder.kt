package com.metalbear.mirrord

import com.intellij.openapi.ui.DialogBuilder
import com.intellij.ui.components.JBList
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.GridBagLayout
import java.awt.GridLayout
import javax.swing.*
import javax.swing.border.EmptyBorder


class MirrordDialogBuilder {
    private val dialogHeading: String = "mirrord"
    private val podLabel: JLabel = JLabel("Select pod to impersonate")
    private val namespaceLabel: JLabel = JLabel("Select Namespace to use")
    private val optionLabel: JLabel = JLabel("Options")

    fun createMirrordKubeDialog(pods: JBList<String>, fileOpsCheckbox: JCheckBox, remoteDnsCheckbox: JCheckBox, ephemeralCheckbox: JCheckBox, agentRustLog: JTextField, rustLog: JTextField): JPanel {
        val dialogPanel = JPanel(BorderLayout())
        podLabel.border = EmptyBorder(5, 40, 5, 5)

        val podPanel = JPanel(GridLayout(2, 1, 10, 5))
        podPanel.add(podLabel, BorderLayout.NORTH)
        podPanel.add(pods)

        dialogPanel.add(podPanel, BorderLayout.WEST)

        dialogPanel.add(JSeparator(JSeparator.VERTICAL),
                BorderLayout.CENTER)

        val optionsPanel = JPanel(GridLayout(6, 1, 10, 2))
        optionLabel.border = EmptyBorder(5, 110, 5, 20)

        optionsPanel.add(optionLabel)
        optionsPanel.add(fileOpsCheckbox)
        optionsPanel.add(remoteDnsCheckbox)
        optionsPanel.add(ephemeralCheckbox)

        val agentLogPanel = JPanel(GridBagLayout())
        agentLogPanel.add(JLabel("Agent Log Level: "))
        agentRustLog.size = Dimension(5, 5)
        agentLogPanel.add(agentRustLog)

        agentLogPanel.border = EmptyBorder(10, 10, 10, 10)

        val rustLogPanel = JPanel(GridBagLayout())
        rustLogPanel.add(JLabel("Layer Log Level: "))
        rustLog.size = Dimension(5, 5)
        rustLogPanel.add(rustLog)

        rustLogPanel.border = EmptyBorder(10, 10, 10, 10)

        optionsPanel.add(agentLogPanel)
        optionsPanel.add(rustLogPanel)

        dialogPanel.add(optionsPanel, BorderLayout.EAST)

        return dialogPanel
    }

    fun createMirrordNamespaceDialog(namespaces: JBList<String>): JPanel {
        val dialogPanel = JPanel(BorderLayout())
        namespaceLabel.border = EmptyBorder(5, 20, 5, 20)
        dialogPanel.add(namespaceLabel, BorderLayout.NORTH)
        dialogPanel.add(namespaces, BorderLayout.SOUTH)
        return dialogPanel
    }

    fun getDialogBuilder(dialogPanel: JPanel): DialogBuilder {
        val dialogBuilder = DialogBuilder()

        dialogBuilder.setCenterPanel(dialogPanel)
        dialogBuilder.setTitle(dialogHeading)

        return dialogBuilder
    }
}