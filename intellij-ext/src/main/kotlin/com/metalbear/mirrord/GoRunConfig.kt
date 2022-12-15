package com.metalbear.mirrord

import com.goide.execution.GoRunConfigurationBase
import com.goide.execution.GoRunningState
import com.goide.execution.extension.GoRunConfigurationExtension
import com.goide.util.GoExecutor
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.TargetedCommandLineBuilder
import com.intellij.openapi.application.PathManager
import java.nio.file.Paths
import com.intellij.openapi.util.SystemInfo

class GoRunConfig : GoRunConfigurationExtension() {
    companion object {
        fun clearGoEnv() {
            for (key in MirrordListener.mirrordEnv.keys) {
                if (goCmdLine?.getEnvironmentVariable(key) != null) {
                    goCmdLine?.removeEnvironmentVariable(key)
                }
            }
        }

        var goCmdLine: TargetedCommandLineBuilder? = null
    }

    override fun isApplicableFor(configuration: GoRunConfigurationBase<*>): Boolean {
        return true
    }

    override fun isEnabledFor(
        applicableConfiguration: GoRunConfigurationBase<*>,
        runnerSettings: RunnerSettings?
    ): Boolean {
        return true
    }

    override fun patchCommandLine(
        configuration: GoRunConfigurationBase<*>,
        runnerSettings: RunnerSettings?,
        cmdLine: TargetedCommandLineBuilder,
        runnerId: String,
        state: GoRunningState<out GoRunConfigurationBase<*>>,
        commandLineType: GoRunningState.CommandLineType
    ) {
        if (commandLineType == GoRunningState.CommandLineType.RUN && MirrordListener.enabled && !MirrordListener.envSet) {
            goCmdLine = cmdLine
            MirrordListener.mirrordEnv["MIRRORD_SKIP_PROCESSES"] = "dlv;debugserver"

            for ((key, value) in MirrordListener.mirrordEnv) {
                cmdLine.addEnvironmentVariable(key, value)
            }
            MirrordListener.envSet = true
        }
        super.patchCommandLine(configuration, runnerSettings, cmdLine, runnerId, state, commandLineType)
    }

    override fun patchExecutor(
        configuration: GoRunConfigurationBase<*>,
        runnerSettings: RunnerSettings?,
        executor: GoExecutor,
        runnerId: String,
        state: GoRunningState<out GoRunConfigurationBase<*>>,
        commandLineType: GoRunningState.CommandLineType
    ) {
        if (commandLineType == GoRunningState.CommandLineType.RUN &&
            MirrordListener.enabled && !MirrordListener.envSet &&
            SystemInfo.isMac &&
            !MirrordListener.defaultFlow
        ) {
            val delvePath = getCustomDelvePath()
            // convert the delve file to an executable
            val delveExecutable = Paths.get(delvePath).toFile()
            if (delveExecutable.exists()) {
                if (!delveExecutable.canExecute()) {
                    delveExecutable.setExecutable(true)
                }
                executor.withExePath(delvePath)
            }
        }
        super.patchExecutor(configuration, runnerSettings, executor, runnerId, state, commandLineType)
    }

    private fun getCustomDelvePath(): String {
        return MirrordPathManager.getBinary("dlv", false)!!
    }
}