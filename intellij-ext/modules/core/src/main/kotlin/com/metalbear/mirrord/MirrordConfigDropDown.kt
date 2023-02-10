package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import javax.swing.JComponent

class MirrordConfigDropDown : ComboBoxAction() {
    private lateinit var configPaths: ArrayList<String>
    companion object {
        var selectedConfig = "Select Configuration"
    }

    override fun createPopupActionGroup(button: JComponent?): DefaultActionGroup {
        val actionGroup = DefaultActionGroup()
        configPaths.forEach { configPath ->
            val trimmedPath = configPath.split("/").takeLast(3).joinToString("/")
            actionGroup.add(object : AnAction(configPath) {
                override fun actionPerformed(e: AnActionEvent) {
                    selectedConfig = trimmedPath
                    e.presentation.text = selectedConfig
                }
            })
        }
        return actionGroup
    }

    override fun update(e: AnActionEvent) {
        val project = e.project ?: throw Error("required to have configured project to use mirrord config.")
        configPaths = MirrordConfigAPI.searchConfigPaths(project) as ArrayList<String>
        if (configPaths.size > 1) {
            configPaths.add(0, "Default Configuration")
        }
        e.presentation.isEnabledAndVisible = configPaths.size > 1
        e.presentation.text = selectedConfig
    }
}
