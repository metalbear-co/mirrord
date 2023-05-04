package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.DataContext
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.psi.search.FilenameIndex
import javax.swing.JComponent
import com.intellij.openapi.actionSystem.CommonDataKeys

class MirrordConfigDropDown : ComboBoxAction() {

    override fun getActionUpdateThread() = ActionUpdateThread.BGT
    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        val currentProject = CommonDataKeys.PROJECT.getData(dataContext) ?: throw Error("No project found")
        var allFiles = FilenameIndex.getAllFilenames(currentProject)
//        var mirrordConfigFiles = FilenameIndex.getAllFilesByExt(currentProject, "mirrord")
//        var mirrordConfigFileNames = mirrordConfigFiles.map { it.name }
        return DefaultActionGroup()
    }
}
