package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent

class MirrordGotoConfigAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: throw Error("required to have configured project to use mirrord config.")
        MirrordConfigAPI.openConfig(project)
    }
}