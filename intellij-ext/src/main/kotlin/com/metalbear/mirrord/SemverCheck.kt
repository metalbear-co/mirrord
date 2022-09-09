package com.metalbear.mirrord

import com.intellij.notification.NotificationType
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManagerListener
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.extensions.PluginId
import java.net.URL
import com.github.zafarkhaja.semver.Version;

class SemverCheck : ProjectManagerListener {
    private val version: String? = PluginManagerCore.getPlugin(PluginId.getId("com.metalbear.mirrord"))?.version
    private val versionCheckEndpoint: String = "https://version.mirrord.dev/get-latest-version?source=1&version=$version"
    private val semverCheckEnabled: Boolean = true

    override fun projectOpened(project: Project) {
        if (semverCheckEnabled) {
            checkVersion(project)
        }
        super.projectOpened(project)
    }

    private fun checkVersion(project: Project) {
        val remoteVersion = Version.valueOf(URL(versionCheckEndpoint).readText())
        val localVersion = Version.valueOf(version)
        if (localVersion.lessThan(remoteVersion)) {
            // TODO: fix this to open the plugins window
            MirrordEnabler.notify("New version of mirrord $remoteVersion is available! Update: Preferences->Plugins->mirrord", NotificationType.INFORMATION, project)
        }
        return
    }
}