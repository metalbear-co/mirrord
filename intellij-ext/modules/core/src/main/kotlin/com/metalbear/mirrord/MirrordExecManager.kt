package com.metalbear.mirrord

import com.intellij.execution.wsl.WSLDistribution
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.SystemInfo
import com.intellij.util.io.exists
import kotlinx.collections.immutable.toImmutableMap

/**
 * Functions to be called when one of our entry points to the program is called - when process is
 * launched, when go entrypoint, etc It will check to see if it already occured for current run and
 * if it did, it will do nothing
 */
object MirrordExecManager {
    var enabled: Boolean = false

    private fun chooseTarget(wslDistribution: WSLDistribution?, project: Project): String? {
        val pods =
            MirrordApi.listPods(
                MirrordConfigAPI.getNamespace(project),
                project,
                wslDistribution
            )
        return MirrordExecDialog.selectTargetDialog(pods)
    }

    private fun chooseConfig(project: Project): String? {
        val configPaths = MirrordConfigAPI.searchConfigPaths(project)
        return if (configPaths.size > 1) {
            MirrordExecDialog.selectConfigDialog(configPaths)
        } else {
            getConfigPath(project)
        }
    }

    private fun getConfigPath(project: Project): String? {
        val configPath = MirrordConfigAPI.getConfigPath(project)
        return if (configPath.exists()) {
            configPath.toAbsolutePath().toString()
        } else {
            null
        }
    }

    /** Starts mirrord, shows dialog for selecting pod if target not set and returns env to set. */
    fun start(wslDistribution: WSLDistribution?, project: Project): Map<String, String>? {
        if (!enabled) {
            return null
        }
        if (SystemInfo.isWindows && wslDistribution == null) {
            MirrordNotifier.errorNotification("Can't use mirrord on Windows without WSL", project)
            return null
        }

        MirrordVersionCheck.checkVersion(project)

        var target: String? = null
        var config: String? = null
        if (!MirrordConfigAPI.isTargetSet(project)) {
            // In some cases, we're executing from a `ReadAction` context, which means we
            // can't block and wait for a WriteAction (such as invokeAndWait).
            // Executing it in a thread pool seems to fix, fml.
            ApplicationManager.getApplication()
                .executeOnPooledThread {
                    ApplicationManager.getApplication().invokeAndWait() {
                        target = chooseTarget(wslDistribution, project)
                        config = chooseConfig(project)
                    }
                }
                .get()

            if (target == null) {
                MirrordNotifier.progress("mirrord loading canceled.", project)
                return null
            }
        }

        var env = MirrordApi.exec(target, getConfigPath(project), project, wslDistribution)

        env["DEBUGGER_IGNORE_PORTS_PATCH"] = "45000-65535"
        return env.toImmutableMap()
    }
}
