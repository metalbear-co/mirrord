package com.metalbear.mirrord

import com.github.zafarkhaja.semver.Version
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationType
import com.intellij.ide.util.PropertiesComponent
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManagerListener
import com.intellij.openapi.updateSettings.impl.pluginsAdvertisement.installAndEnable
import com.sun.istack.NotNull
import java.net.URL

class SemverCheck : ProjectManagerListener {
    private val pluginId = PluginId.getId("com.metalbear.mirrord")
    private val version: String? = PluginManagerCore.getPlugin(pluginId)?.version
    private val versionCheckEndpoint: String =
        "https://version.mirrord.dev/get-latest-version?source=3&version=$version"
    private var semverCheckEnabled
        get() = PropertiesComponent.getInstance().getBoolean("semverCheckEnabled", true)
        set(value) {
            PropertiesComponent.getInstance().setValue("semverCheckEnabled", value)
        }

    override fun projectOpened(project: Project) {
        if (semverCheckEnabled) {
            checkVersion(project)
        }
        super.projectOpened(project)
    }

    private fun checkVersion(project: Project) {
        val remoteVersion = Version.valueOf(URL(versionCheckEndpoint).readText())
        val localVersion = Version.valueOf("2.3.1")
        if (localVersion.lessThan(remoteVersion)) {
            MirrordEnabler.notifier(
                "Your version of mirrord is outdated, you should update.",
                NotificationType.INFORMATION
            )
                .addAction(object : NotificationAction("Update") {
                    override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                        installAndEnable(
                            project,
                            setOf<@receiver:NotNull PluginId>(pluginId),
                            true
                        ) {
                            notification.expire()
                        }
                    }
                })
                .addAction(object : NotificationAction("Don't show again") {
                    override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                        semverCheckEnabled = false
                        notification.expire()
                    }
                }).notify(project)
        }
        return
    }
}
