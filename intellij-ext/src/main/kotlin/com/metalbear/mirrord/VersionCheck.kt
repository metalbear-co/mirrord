package com.metalbear.mirrord

import com.github.zafarkhaja.semver.Version
import com.intellij.ide.plugins.PluginManagerConfigurable
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.util.PropertiesComponent
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManagerListener
import java.net.URL


class VersionCheck : ProjectManagerListener {
    private val pluginId = PluginId.getId("com.metalbear.mirrord")
    private val version: String? = PluginManagerCore.getPlugin(pluginId)?.version
    private val versionCheckEndpoint: String =
        "https://version.mirrord.dev/get-latest-version?source=3&version=$version"
    private var versionCheckDisabled
        get() = PropertiesComponent.getInstance().getBoolean("versionCheckDisabled", false)
        set(value) {
            PropertiesComponent.getInstance().setValue("versionCheckDisabled", value)
        }

    override fun projectOpened(project: Project) {
        if (!versionCheckDisabled) {
            checkVersion(project)
        }
        super.projectOpened(project)
    }

    private fun checkVersion(project: Project) {
        val remoteVersion = Version.valueOf(URL(versionCheckEndpoint).readText())
        val localVersion = Version.valueOf(version)
        if (localVersion.lessThan(remoteVersion)) {
            MirrordEnabler.notifier(
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
