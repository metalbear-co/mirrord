package com.metalbear.mirrord

import com.intellij.notification.Notification
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.ToggleAction
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.github.zafarkhaja.semver.Version
import com.intellij.ide.plugins.PluginManagerConfigurable
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.util.PropertiesComponent
import com.intellij.notification.NotificationAction
import com.intellij.openapi.extensions.PluginId
import java.net.URL


@Suppress("DialogTitleCapitalization")
class MirrordEnabler : ToggleAction() {
    companion object {
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
    override fun isSelected(e: AnActionEvent): Boolean {
        return MirrordListener.enabled
    }

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        if (state) {
            notify("mirrord enabled", NotificationType.INFORMATION, e.project)
            e.project?.let { checkVersion(it) }
        } else {
            notify("mirrord disabled", NotificationType.INFORMATION, e.project)
        }

        MirrordListener.enabled = state
    }

    private val pluginId = PluginId.getId("com.metalbear.mirrord")
    private val version: String? = PluginManagerCore.getPlugin(pluginId)?.version
    private val versionCheckEndpoint: String =
        "https://version.mirrord.dev/get-latest-version?source=3&version=$version"
    private var versionCheckDisabled
        get() = PropertiesComponent.getInstance().getBoolean("versionCheckDisabled", false)
        set(value) {
            PropertiesComponent.getInstance().setValue("versionCheckDisabled", value)
        }

    private fun checkVersion(project: Project) {
        val remoteVersion = Version.valueOf(URL(versionCheckEndpoint).readText())
        val localVersion = Version.valueOf(version)
        if (localVersion.lessThan(remoteVersion)) {
            notifier(
                "Your version of mirrord is outdated, you should update.",
                NotificationType.INFORMATION
            )
                .addAction(object : NotificationAction("Update") {
                    override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                        try {
                            PluginManagerConfigurable.showPluginConfigurable(project, listOf(pluginId))
                        } catch (e: Exception) {
                            notification.expire()
                        }
                        notification.expire()
                    }
                })
                .addAction(object : NotificationAction("Don't show again") {
                    override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                        versionCheckDisabled = true
                        notification.expire()
                    }
                }).notify(project)
        }
        return
    }
}