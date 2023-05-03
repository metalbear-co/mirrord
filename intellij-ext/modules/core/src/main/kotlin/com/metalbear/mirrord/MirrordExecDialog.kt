package com.metalbear.mirrord

import com.intellij.openapi.ui.DialogBuilder
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import java.awt.*
import java.awt.event.KeyEvent
import java.awt.event.KeyListener
import javax.swing.*
import javax.swing.border.EmptyBorder
import javax.swing.event.DocumentEvent
import javax.swing.event.DocumentListener


object MirrordExecDialog {
    private const val dialogHeading: String = "mirrord"
    private const val targetLabel = "Select Target"

    fun selectTargetDialog(targets: List<String>): String? {
        var targets = targets.sorted().toMutableList()
        MirrordSettingsState.instance.mirrordState.lastChosenTarget?.let {
            val idx = targets.indexOf(it)
            if (idx != -1) {
                targets.removeAt(idx)
                targets.add(0, it)
            }
        }
        val jbTargets = targets.asJBList()
        val searchField = JTextField()
        searchField.document.addDocumentListener(object : DocumentListener {
            override fun insertUpdate(e: DocumentEvent) = updateList()
            override fun removeUpdate(e: DocumentEvent) = updateList()
            override fun changedUpdate(e: DocumentEvent) = updateList()

            private fun updateList() {
                val searchTerm = searchField.text
                val filteredTargets = targets.filter { it.contains(searchTerm, true) }.sorted()
                jbTargets.setListData(filteredTargets.toTypedArray())
            }

        })

        // Add focus logic then we can change back and forth from search field
        // to target selection using tab/shift+tab
        searchField.addKeyListener(object : KeyListener {
            override fun keyTyped(p0: KeyEvent?) {
            }

            override fun keyPressed(e: KeyEvent?) {
                if (e?.keyCode === KeyEvent.VK_TAB) {
                    if (e.modifiersEx > 0) {
                        searchField.transferFocusBackward()
                    } else {
                        searchField.transferFocus()
                    }
                    e.consume()
                }
            }

            override fun keyReleased(p0: KeyEvent?) {
            }
        })
        val result = DialogBuilder().apply {
            setCenterPanel(JPanel(BorderLayout()).apply {
                add(createSelectionDialog(targetLabel, jbTargets, searchField), BorderLayout.WEST)
            })
            setTitle(dialogHeading)
        }.show()
        if (result == DialogWrapper.OK_EXIT_CODE && !jbTargets.isSelectionEmpty) {
            val selectedValue = jbTargets.selectedValue
            MirrordSettingsState.instance.mirrordState.lastChosenTarget = selectedValue
            return selectedValue
        }
        return null
    }

    private fun List<String>.asJBList() = JBList(this).apply {
        selectionMode = ListSelectionModel.SINGLE_SELECTION
    }

    private fun createSelectionDialog(label: String, items: JBList<String>, searchField: JTextField): JPanel =
        JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            border = EmptyBorder(10, 5, 10, 5)
            add(JLabel(label).apply {
                alignmentX = JLabel.LEFT_ALIGNMENT
            })
            add(Box.createRigidArea(Dimension(0, 5)))
            add(searchField.apply {
                alignmentX = JBScrollPane.LEFT_ALIGNMENT
                preferredSize = Dimension(250, 30)
                size = Dimension(250, 30)
            })
            add(JBScrollPane(items).apply {
                alignmentX = JBScrollPane.LEFT_ALIGNMENT
                preferredSize = Dimension(250, 350)
                size = Dimension(250, 350)
            })
        }


}
