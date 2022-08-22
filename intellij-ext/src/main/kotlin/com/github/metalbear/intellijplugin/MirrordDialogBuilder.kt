package com.github.metalbear.intellijplugin

import com.intellij.openapi.ui.DialogBuilder
import com.intellij.ui.components.JBList
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.GridBagLayout
import java.awt.GridLayout
import javax.swing.*
import javax.swing.border.EmptyBorder


class MirrordDialogBuilder {
    private val heading: String = "mirrord"
    private val labelName: String = "Select pod to impersonate"

    fun createMirrordKubeDialog(pods: JBList<String>, fileOpsCheckbox: JCheckBox, remoteDnsCheckbox: JCheckBox, agentRustLog: JTextField, rustLog: JTextField): JPanel {
        val dialogPanel = JPanel(BorderLayout())
        val label = JLabel(labelName)

        val podPanel = JPanel(GridLayout(2, 1, 10, 5))
        podPanel.add(label, BorderLayout.NORTH)
        podPanel.add(pods)

        dialogPanel.add(podPanel, BorderLayout.WEST)

        dialogPanel.add(JSeparator(JSeparator.VERTICAL),
                BorderLayout.CENTER)

        val checkBoxPanel = JPanel(GridLayout(4, 1, 10, 2))
        checkBoxPanel.add(fileOpsCheckbox)
        checkBoxPanel.add(remoteDnsCheckbox)

        val agentLogPanel = JPanel(GridBagLayout())
        agentLogPanel.add(JLabel("MIRRORD_AGENT_RUST_LOG"))
        agentRustLog.size = Dimension(5, 5)
        agentLogPanel.add(agentRustLog)

        agentLogPanel.border = EmptyBorder(10, 10, 10, 10);

        val rustLogPanel = JPanel(GridBagLayout())
        rustLogPanel.add(JLabel("RUST_LOG"))
        rustLog.size = Dimension(5, 5)
        rustLogPanel.add(rustLog)

        rustLogPanel.border = EmptyBorder(10, 10, 10, 10);

        checkBoxPanel.add(agentLogPanel)
        checkBoxPanel.add(rustLogPanel)

        dialogPanel.add(checkBoxPanel, BorderLayout.EAST)

        return dialogPanel
    }

    fun createMirrordNamespaceDialog(namespaces: JBList<String>): JPanel {
        val dialogPanel = JPanel(BorderLayout())
        val label = JLabel("Select Namespace to use")
        val border = EmptyBorder(5, 20, 5, 20)
        label.border = border
        dialogPanel.add(label, BorderLayout.NORTH)
        dialogPanel.add(namespaces, BorderLayout.SOUTH)
        return dialogPanel
    }

    fun getDialogBuilder(dialogPanel: JPanel): DialogBuilder {
        val dialogBuilder = DialogBuilder()

        dialogBuilder.setCenterPanel(dialogPanel)
        dialogBuilder.setTitle(heading)

        return dialogBuilder
    }
}