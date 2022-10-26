package com.metalbear.mirrord

import com.intellij.openapi.ui.DialogBuilder
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.GridBagLayout
import java.awt.GridLayout
import javax.swing.*
import javax.swing.border.EmptyBorder


object MirrordDialogBuilder {
    private const val dialogHeading: String = "mirrord"
    private const val podLabel = "Select Pod"
    private const val namespaceLabel = "Select Namespace"

    fun createDialogBuilder(dialogPanel: JPanel): DialogBuilder = DialogBuilder().apply {
        setCenterPanel(dialogPanel)
        resizable(false)
        setTitle(dialogHeading)
    }

    fun createMirrordNamespaceDialog(namespaces: JBList<String>): JPanel = JPanel(BorderLayout()).apply {
        add(createSelectionDialog(namespaceLabel, namespaces), BorderLayout.CENTER)
    }

    fun createMirrordConfigDialog(
        pods: JBList<String>,
        fileOps: JCheckBox,
        trafficStealing: JCheckBox,
        ephemeralCheckbox: JCheckBox,
        remoteDns: JCheckBox,
        tcpOutgoingTraffic: JCheckBox,
        udpOutgoingTraffic: JCheckBox,
        agentRustLog: JComboBox<LogLevel>,
        rustLog: JComboBox<LogLevel>,
        excludeEnv: JTextField,
        includeEnv: JTextField
    ): JPanel = JPanel(BorderLayout()).apply {
        add(createSelectionDialog(podLabel, pods), BorderLayout.WEST)
        add(JSeparator(JSeparator.VERTICAL), BorderLayout.CENTER)
        add(JPanel(GridLayout(5, 2, 15, 2)).apply {
            border = EmptyBorder(0, 5, 5, 5)
            add(fileOps)
            add(trafficStealing)
            add(ephemeralCheckbox)
            add(remoteDns)
            add(tcpOutgoingTraffic)
            add(udpOutgoingTraffic)
            add(JPanel(GridBagLayout()).apply {
                add(JLabel("Agent Log Level:"))
                add(agentRustLog)
            })
            add(JPanel(GridBagLayout()).apply {
                add(JLabel("Layer Log Level:"))
                add(rustLog)
            })
            add(JPanel(GridLayout(2, 1)).apply {
                add(JLabel("Exclude env vars:"))
                add(excludeEnv)
            })
            add(JPanel(GridLayout(2, 1)).apply {
                add(JLabel("Include env vars:"))
                add(includeEnv)
            })
        }, BorderLayout.EAST)
    }

    private fun createSelectionDialog(label: String, items: JBList<String>): JPanel =
        JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            border = EmptyBorder(10, 5, 10, 5)
            add(JLabel(label).apply {
                alignmentX = JLabel.LEFT_ALIGNMENT
            })
            add(Box.createRigidArea(Dimension(0, 5)))
            add(JBScrollPane(items).apply {
                alignmentX = JBScrollPane.LEFT_ALIGNMENT
                preferredSize = Dimension(250, 350)
            })
        }
}