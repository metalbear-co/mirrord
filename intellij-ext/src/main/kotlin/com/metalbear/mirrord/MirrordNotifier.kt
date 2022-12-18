package com.metalbear.mirrord

import com.intellij.notification.Notification
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project

/**
 * Mirrord notification handler
 */
object MirrordNotifier {
    private val notificationManager: NotificationGroup = NotificationGroupManager
        .getInstance()
        .getNotificationGroup("mirrord Notification Handler")

    fun notify(message: String, type: NotificationType, project: Project?) {
        ApplicationManager.getApplication().invokeLater {
            notificationManager
                .createNotification("mirrord", message, type)
                .notify(project)
        }
    }

    fun notifier(message: String, type: NotificationType): Notification {
        return notificationManager.createNotification("mirrord", message, type)
    }
}