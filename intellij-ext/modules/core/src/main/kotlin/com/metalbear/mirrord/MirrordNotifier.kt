package com.metalbear.mirrord

import com.intellij.ide.BrowserUtil
import com.intellij.notification.*
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.NlsContexts
import com.intellij.openapi.wm.ToolWindowManager
import java.util.concurrent.atomic.AtomicReference
import java.util.function.Consumer

/**
 * Mirrord notification handler
 */
object MirrordNotifier {
    private val notificationManager: NotificationGroup = NotificationGroupManager
        .getInstance()
        .getNotificationGroup("mirrord Notification Handler")

    private val progressIndicator = ProgressManager.getInstance().progressIndicator

    fun notify(message: String, type: NotificationType, project: Project?) {
        ApplicationManager.getApplication().invokeLater {
            notificationManager
                .createNotification("mirrord", message, type)
                .notify(project)
        }
    }

    fun progress(message: String) {
        progressIndicator.text = message
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


class SingletonNotificationManager(groupId: String, private val type: NotificationType) {
    private val group = NotificationGroupManager.getInstance().getNotificationGroup(groupId)
    private val notification = AtomicReference<Notification>()
    fun notify(@NlsContexts.NotificationTitle title: String,
               @NlsContexts.NotificationContent content: String,
               project: Project?,
               customizer: Consumer<Notification>
    ) {
        val oldNotification = notification.get()
        if (oldNotification != null) {
            if (isVisible(oldNotification, project)) {
                Thread.sleep(1000)
            }
            oldNotification.expire()
        }

        val newNotification = object : Notification(group.displayId, title, content, type) {
            override fun expire() {
                super.expire()
                notification.compareAndSet(this, null)
            }
        }
        customizer.accept(newNotification)

        if (notification.compareAndSet(oldNotification, newNotification)) {
            newNotification.notify(project)
        }
        else {
            newNotification.expire()
        }
    }

    private fun isVisible(notification: Notification, project: Project?): Boolean {
        val balloon = when {
            group.displayType != NotificationDisplayType.TOOL_WINDOW -> notification.balloon
            project != null -> ToolWindowManager.getInstance(project).getToolWindowBalloon(group.toolWindowId!!)
            else -> null
        }
        return balloon != null && !balloon.isDisposed
    }
}
