package com.metalbear.mirrord

import com.github.zafarkhaja.semver.Version
import com.intellij.ide.plugins.PluginManagerConfigurable
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.util.PropertiesComponent
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.project.Project
import java.net.URL
import java.time.LocalDateTime
import java.time.ZoneOffset

object MirrordVersionCheck {
    private val pluginId = PluginId.getId("com.metalbear.mirrord")
    private val version: String? = PluginManagerCore.getPlugin(pluginId)?.version
    private val os: String = System.getProperty("os.name").replace(" ", "%20")
    private val versionCheckEndpoint: String =
        "https://version.mirrord.dev/get-latest-version?source=3&version=$version&platform=$os"
    private const val LAST_CHECK_KEY = "lastCheck"

    /**
     * Fetch the latest version number, compare to local version. If there is a later version available, notify.
     * Return early without checking if already performed full check in the last 3 minutes.
     */
    fun checkVersion(project: Project) {
        val pc = PropertiesComponent.getInstance() // Don't pass project, to get ide-wide persistence.
        val lastCheckEpoch = pc.getLong(LAST_CHECK_KEY, 0)
        val nowUTC = LocalDateTime.now(ZoneOffset.UTC)
        val lastCheckUTCDateTime = LocalDateTime.ofEpochSecond(lastCheckEpoch, 0, ZoneOffset.UTC)
        if (lastCheckUTCDateTime.isAfter(nowUTC.minusMinutes(3))) {
            return // Already checked in the last 3 hours. Don't check again yet.
        }
        val nowEpoch = nowUTC.toEpochSecond(ZoneOffset.UTC)
        pc.setValue(LAST_CHECK_KEY, nowEpoch.toString())
        val remoteVersion = Version.valueOf(URL(versionCheckEndpoint).readText())

        // Don't show user anything
        if (!MirrordSettingsState.instance.mirrordState.versionCheckEnabled) {
            return
        }


        val localVersion = Version.valueOf(version)
        if (localVersion.lessThan(remoteVersion)) {
            ApplicationManager.getApplication().invokeLater {
                MirrordNotifier.notifier(
                    "The version of the mirrord plugin is outdated. Would you like to update it now?",
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
                            MirrordSettingsState.instance.mirrordState.versionCheckEnabled = false
                            notification.expire()
                        }
                    }).notify(project)
            }
        }
        return
    }
}