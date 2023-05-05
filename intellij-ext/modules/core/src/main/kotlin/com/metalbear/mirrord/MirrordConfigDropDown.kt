package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.project.Project
import com.intellij.psi.search.FilenameIndex
import javax.swing.JComponent
import com.intellij.psi.search.GlobalSearchScope

class MirrordConfigDropDown : ComboBoxAction() {

    private var configFiles = mutableListOf<String>()
    override fun getActionUpdateThread() = ActionUpdateThread.BGT
    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        val currentProject = CommonDataKeys.PROJECT.getData(dataContext) ?: throw Error("No project found")
        configFiles = findPaths(currentProject)

        val actions = configFiles.map { configPath ->
            object : AnAction(configPath) {
                override fun actionPerformed(e: AnActionEvent) {

                }
            }
        }
        return DefaultActionGroup().apply { addAll(actions) }
    }

    override fun update(e: AnActionEvent)  {
        e.presentation.apply {

        }
    }
    private fun findPaths(project: Project): MutableList<String> {
        var mirrordConfigFiles = FilenameIndex.getAllFilesByExt(project, "json")
        var mirrordConfigFileNames = mirrordConfigFiles.map { it.path }.filter { it.contains("mirrord") }
        return mirrordConfigFileNames.toMutableList()
    }
}
