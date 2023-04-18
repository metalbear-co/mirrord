package com.metalbear.mirrord

import com.intellij.ide.BrowserUtil
import com.intellij.notification.*
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project

/**
 * Mirrord notification handler
 */
object MirrordNotifier {
    private val notificationManager: NotificationGroup = NotificationGroupManager
        .getInstance()
        .getNotificationGroup("mirrord Notification Handler")

    private val progressNotificationManager = SingletonNotificationManager("mirrord Notification Handler", NotificationType.INFORMATION)

    fun notify(message: String, type: NotificationType, project: Project?) {
        ApplicationManager.getApplication().invokeLater {
            notificationManager
                .createNotification("mirrord", message, type)
                .notify(project)
        }
    }

    fun progress(message: String, project: Project?) {
        ApplicationManager.getApplication().invokeLater {
            progressNotificationManager.notify("mirrord", message, project) {}
        }
    }
    fun notifier(message: String, type: NotificationType): Notification {
        return notificationManager.createNotification("mirrord", message, type)
    }

    fun errorNotification(message: String, project: Project?) {
        ApplicationManager.getApplication().invokeLater {
            notifier(message, NotificationType.ERROR).addAction(object : NotificationAction("Get support on Discord") {
                override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                    BrowserUtil.browse("https://discord.gg/metalbear")
                    notification.expire()
                }
            }).addAction(object : NotificationAction("Report on GitHub") {
                override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                    BrowserUtil.browse("https://github.com/metalbear-co/mirrord/issues/new?assignees=&labels=bug&template=bug_report.yml")
                    notification.expire()
                }
            }).notify(project)
        }
    }
}