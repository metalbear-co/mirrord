package com.metalbear.mirrord

import com.intellij.execution.wsl.WSLDistribution
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.SystemInfo
import com.intellij.util.io.exists
import kotlinx.collections.immutable.toImmutableMap

/**
 * Functions to be called when one of our entry points to the program is called - when process is
 * launched, when go entrypoint, etc. It will check to see if it already occurred for current run and
 * if it did, it will do nothing
 */
object MirrordExecManager {
    var enabled: Boolean = false

    private fun chooseTarget(wslDistribution: WSLDistribution?, project: Project): String? {
        MirrordLogger.logger.debug("choose target called")
        val path = MirrordConfigAPI.getConfigPath(project)
        val configPath = when(path.exists())
        {
            true -> path.toString()
            false -> null
        }

        val pods =
                MirrordApi.listPods(
                        configPath,
                        project,
                        wslDistribution
                )
        MirrordLogger.logger.debug("returning pods")
        return MirrordExecDialog.selectTargetDialog(pods)
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
            MirrordLogger.logger.debug("disabled, returning")
            return null
        }
        if (SystemInfo.isWindows && wslDistribution == null) {
            MirrordNotifier.errorNotification("Can't use mirrord on Windows without WSL", project)
            return null
        }

        MirrordLogger.logger.debug("version check trigger")
        MirrordVersionCheck.checkVersion(project)

        MirrordLogger.logger.debug("target selection")
        var target: String? = null
        if (!MirrordConfigAPI.isTargetSet(project)) {
            val application = ApplicationManager.getApplication()
            MirrordLogger.logger.debug("target not selected, showing dialog")
            // In some cases, we're executing from a `ReadAction` context, which means we
            // can't block and wait for a WriteAction (such as invokeAndWait).
            // Executing it in a thread pool seems to fix, fml.
            // Update: We found out that if we're on DispatchThread we can just
            // run our function, and if we don't we get into a deadlock.
            // I have yet come to understand what exactly is going on. fmlv2
            if (application.isDispatchThread) {
                MirrordLogger.logger.debug("Running from current thread")
                target = chooseTarget(wslDistribution, project)
            }
            else {
                application.executeOnPooledThread {
                    MirrordLogger.logger.debug("executing on pooled thread")
                    application.invokeAndWait {
                        MirrordLogger.logger.debug("choosing target from invoke")
                        target = chooseTarget(wslDistribution, project)
                    }
                }.get()
            }
            if (target == null) {
                MirrordLogger.logger.warn("mirrord loading canceled")
                MirrordNotifier.progress("mirrord loading canceled.", project)
                return null
            }
        }

        val env = MirrordApi.exec(target, getConfigPath(project), project, wslDistribution)

        env["DEBUGGER_IGNORE_PORTS_PATCH"] = "45000-65535"
        return env.toImmutableMap()
    }
}
