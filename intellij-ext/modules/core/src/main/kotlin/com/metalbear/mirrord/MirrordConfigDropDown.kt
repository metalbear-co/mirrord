package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import javax.swing.JComponent

class MirrordConfigDropDown : ComboBoxAction() {
    companion object {
        private var project: Project? = null
        private val configPaths by lazy { arrayListOf<String>() }
        var selectedConfig = "Select Configuration"

        fun updatePaths() {
            project?.let { prj ->
                val paths = MirrordConfigAPI.searchConfigPaths(prj) as? ArrayList<String>
                if (!paths.isNullOrEmpty()) {
                    configPaths.clear()
                    configPaths.addAll(paths)
                    selectedConfig = "Select Configuration"
                }
            }
        }
    }

    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        val actions = configPaths.map { configPath ->
            object : AnAction(configPath) {
                override fun actionPerformed(e: AnActionEvent) {
                    selectedConfig = configPath
                }
            }
        }
        return DefaultActionGroup().apply { addAll(actions) }
    }

    override fun update(e: AnActionEvent) {
        e.presentation.apply {
            project = project ?: e.project
            if (configPaths.isEmpty()) {
                updatePaths()
            }
            isVisible = when (configPaths.size) {
                0, 1 -> false
                else -> true
            }
            text = selectedConfig
        }
    }
}

class MirrordConfigFileRefresher : BulkFileListener {
    override fun before(events: MutableList<out VFileEvent>) {
        MirrordConfigDropDown.updatePaths()
    }
}
