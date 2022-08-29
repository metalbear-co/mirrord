package com.metalbear.mirrord

import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.ToggleAction

@Suppress("DialogTitleCapitalization")
class MirrordEnabler : ToggleAction() {
    private val notificationManager: NotificationGroup
        get() = NotificationGroupManager
                .getInstance()
                .getNotificationGroup("mirrord Notification Handler")


    override fun isSelected(e: AnActionEvent): Boolean {
        return MirrordListener.enabled
    }

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        if (state) {
            notificationManager
                    .createNotification("mirrord", "mirrord enabled")
                    .notify(e.project)

        } else {
            notificationManager
                    .createNotification("mirrord", "mirrord disabled")
                    .notify(e.project)
        }

        MirrordListener.enabled = state
    }
}