package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import javax.swing.JComponent

class MirrordConfigDropDown : ComboBoxAction() {
    private var project: Project? = null
    private var configPaths: ArrayList<String> = ArrayList()

    companion object {
        var selectedConfig = "Select Configuration"
    }

    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        project = dataContext.getData(CommonDataKeys.PROJECT)
        configPaths = MirrordConfigAPI.searchConfigPaths(project!!) as ArrayList<String>

        return DefaultActionGroup().apply {
            configPaths.forEach { configPath ->
                add(object : AnAction(configPath) {
                    override fun actionPerformed(e: AnActionEvent) {
                        selectedConfig = configPath
                        //  e.presentation.text = selectedConfig
                        //  ^ apparently nothing happens when text is updated here,
                        //  so I guess need to update it in update()?
                    }
                })
            }
        }
    }

    fun updatePaths() {
        project?.let {
            configPaths = MirrordConfigAPI.searchConfigPaths(it) as ArrayList<String>
            selectedConfig = "Select Configuration"
        }
    }

    override fun update(e: AnActionEvent) {
        e.presentation.apply {
            isEnabledAndVisible = configPaths.size > 1
            text = selectedConfig
        }
    }
}

class MirrordConfigFileRefresher : BulkFileListener {
    override fun before(events: MutableList<out VFileEvent>) {
        MirrordConfigDropDown().updatePaths()
    }
}
