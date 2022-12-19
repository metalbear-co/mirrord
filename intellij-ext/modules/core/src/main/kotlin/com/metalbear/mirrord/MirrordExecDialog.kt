package com.metalbear.mirrord

import com.intellij.openapi.ui.DialogBuilder
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import java.awt.*
import javax.swing.*
import javax.swing.border.EmptyBorder


object MirrordExecDialog {
    private const val dialogHeading: String = "mirrord"
    private const val targetLabel = "Select Target"

    fun selectTargetDialog(targets: List<String>): String? {
        val jbTargets = targets.asJBList()
        val result = DialogBuilder(). apply {

            setCenterPanel(JPanel(BorderLayout()).apply {
                add(createSelectionDialog(targetLabel, jbTargets), BorderLayout.WEST)
            })
            setTitle(dialogHeading)
        }.show()
        if (result == DialogWrapper.OK_EXIT_CODE && !jbTargets.isSelectionEmpty) {
            return jbTargets.selectedValue
        }
        return null
    }

    private fun List<String>.asJBList() = JBList(this).apply {
        selectionMode = ListSelectionModel.SINGLE_SELECTION
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
