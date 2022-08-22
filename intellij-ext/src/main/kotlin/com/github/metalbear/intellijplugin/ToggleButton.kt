package com.github.metalbear.intellijplugin

import com.intellij.notification.NotificationGroupManager
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.ToggleAction

@Suppress("DialogTitleCapitalization")
class ToggleButton : ToggleAction() {

    override fun isSelected(e: AnActionEvent): Boolean {
        return MirrordListener.enabled
    }

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        val notificationManager = NotificationGroupManager
                .getInstance()
                .getNotificationGroup("mirrord Notification Handler")
        if (state) {
            notificationManager
                    .createNotification("mirrord", "mirrord configuration is active, current project will run in context of the remote pod when debugged")
                    .notify(e.project)

        } else {
            notificationManager
                    .createNotification("mirrord", "mirrord configuration has been removed")
                    .notify(e.project)
        }

        MirrordListener.enabled = state
    }
}