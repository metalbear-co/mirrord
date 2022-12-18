package com.metalbear.mirrord

import com.goide.execution.GoRunConfigurationBase
import com.goide.execution.GoRunningState
import com.goide.execution.extension.GoRunConfigurationExtension
import com.goide.util.GoExecutor
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.TargetedCommandLineBuilder
import java.nio.file.Paths
import com.intellij.openapi.util.SystemInfo

class GolandRunConfigurationExtension : GoRunConfigurationExtension() {
    companion object {
        fun clearGoEnv() {
            for (env in keysToClear) {
                goCmdLine?.getEnvironmentVariable(env)?.let {
                    goCmdLine?.removeEnvironmentVariable(env)
                }
            }

        }

        val keysToClear = arrayOf("LD_PRELOAD", "DYLD_INSERT_LIBRARIES")
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

            cmdLine.addEnvironmentVariable("MIRRORD_SKIP_PROCESSES", "dlv;debugserver")
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
            MirrordListener.enabled &&
            SystemInfo.isMac &&
            state.isDebug
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