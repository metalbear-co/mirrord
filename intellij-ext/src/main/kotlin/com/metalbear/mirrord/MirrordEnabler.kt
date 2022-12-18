package com.metalbear.mirrord

import com.github.zafarkhaja.semver.Version
import com.intellij.ide.BrowserUtil
import com.intellij.ide.plugins.PluginManagerConfigurable
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.util.PropertiesComponent
import com.intellij.notification.*
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.ToggleAction
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.project.Project
import java.net.URL
import java.time.LocalDateTime
import java.time.ZoneOffset


@Suppress("DialogTitleCapitalization")
class MirrordEnabler : ToggleAction() {

    override fun isSelected(e: AnActionEvent): Boolean {
        return MirrordListener.enabled
    }

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        MirrordApi.exec("asd", "aaa", e.project, null)
        if (state) {
            MirrordNotifier.notify("mirrord enabled", NotificationType.INFORMATION, e.project)
        } else {
            MirrordNotifier.notify("mirrord disabled", NotificationType.INFORMATION, e.project)
        }
        MirrordListener.enabled = state
        if (MirrordSettingsState.telemetryEnabled == null ) {
            telemetryConsent(e.project)
        }
    }

    /**
     * Shows a notification asking for consent to send telemetries
     */
    private fun telemetryConsent(project: Project?) {
        MirrordNotifier.notifier(
            "Allow mirrord to send telemetries",
            NotificationType.INFORMATION
        )
            .addAction(object : NotificationAction("Allow") {
                override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                    MirrordSettingsState.telemetryEnabled = true
                    MirrordSettingsState.versionCheckEnabled = true
                    notification.expire()
                }
            })
            .addAction(object : NotificationAction("More info") {
                override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                    BrowserUtil.browse("https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md")
                }
            })
            .addAction(object : NotificationAction("Deny (will disable version check as well)") {
                override fun actionPerformed(e: AnActionEvent, notification: Notification) {
                    MirrordSettingsState.telemetryEnabled = false
                    MirrordSettingsState.versionCheckEnabled = false
                    notification.expire()
                }
            }).notify(project)
    }


}