@file:Suppress("DialogTitleCapitalization")

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

    private val warningNotificationManager: NotificationGroup = NotificationGroupManager
        .getInstance()
        .getNotificationGroup("mirrord Warning Notification Handler")

    fun notify(message: String, type: NotificationType, project: Project?) {
        ApplicationManager.getApplication().invokeLater {
            val notificationManager = when (type) {
                NotificationType.WARNING -> warningNotificationManager
                else -> notificationManager
            }

            notificationManager
                .createNotification("mirrord", message, type)
                .notify(project)
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
            }).addAction(object : NotificationAction("Send us an email") {
                override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                    BrowserUtil.browse("mailto:hi@metalbear.co")
                    notification.expire()
                }
            }).notify(project)
        }
    }
}