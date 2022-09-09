package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import javax.swing.Icon

//class DonotShowAction : AnAction() {
//    constructor(text: String, description: String) {
//        this(text, description)
//    }
//    override fun actionPerformed(e: AnActionEvent) {
//        TODO("Not yet implemented")
//    }
//


internal abstract class DonotShowAction(text: String, description: String, icon: Icon) :
    AnAction(text, description, icon) {
    companion object {
        fun create(
            text: String,
            description: String,
            icon: Icon,
        ): DonotShowAction {
            return object : DonotShowAction(text, description, icon) {
                override fun actionPerformed(e: AnActionEvent) {

                }
            }
        }
    }
}