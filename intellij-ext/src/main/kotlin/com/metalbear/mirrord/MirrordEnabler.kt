package com.metalbear.mirrord

import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.ToggleAction
import com.intellij.openapi.project.Project

@Suppress("DialogTitleCapitalization")
class MirrordEnabler : ToggleAction() {
    companion object {
        private val notificationManager: NotificationGroup = NotificationGroupManager
                .getInstance()
                .getNotificationGroup("mirrord Notification Handler")

        fun notify(message: String, type: NotificationType, project: Project?) {
            notificationManager
                    .createNotification("mirrord", message, type)
                    .notify(project)
        }
    }


    override fun isSelected(e: AnActionEvent): Boolean {
        return MirrordListener.enabled
    }

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        if (state) {
            notify("mirrord enabled", NotificationType.INFORMATION, e.project)
        } else {
            notify("mirrord disabled", NotificationType.INFORMATION, e.project)
        }

        MirrordListener.enabled = state
    }
}