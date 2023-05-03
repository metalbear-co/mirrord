package com.metalbear.mirrord.products.goland

import com.goide.execution.GoRunConfigurationBase
import com.goide.execution.GoRunningState
import com.goide.execution.extension.GoRunConfigurationExtension
import com.goide.util.GoCommandLineParameter.PathParameter
import com.goide.util.GoExecutor
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.TargetedCommandLineBuilder
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.util.SystemInfo
import com.metalbear.mirrord.MirrordExecManager
import com.metalbear.mirrord.MirrordPathManager
import java.nio.file.Paths

class GolandRunConfigurationExtension : GoRunConfigurationExtension() {

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
        if (commandLineType == GoRunningState.CommandLineType.RUN) {

            val wsl = state.targetEnvironmentRequest?.let {
                if (it is WslTargetEnvironmentRequest) {
                    it.configuration.distribution
                } else {
                    null
                }
            }
            val project = configuration.getProject()

            MirrordExecManager.start(wsl, project)?.let {
                env ->
                for (entry in env.entries.iterator()) {
                    cmdLine.addEnvironmentVariable(entry.key, entry.value)
                }
                cmdLine.addEnvironmentVariable("MIRRORD_SKIP_PROCESSES", "dlv;debugserver;go")
            }

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
            MirrordExecManager.enabled &&
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
                // parameters returns a copy, and we can't modify it so need to reset it.
                val patchedParameters = executor.parameters
                for (i in 0 until patchedParameters.size) {
                    // no way to reset the whole commandline, so we just remove each entry.
                    executor.withoutParameter(0)
                    val value = patchedParameters[i]
                    if (value.toPresentableString().endsWith("/dlv", true)) {
                        patchedParameters[i] = PathParameter(delveExecutable.toString())
                    }
                }
                executor.withParameters(*patchedParameters.toTypedArray())
            }

        }
        super.patchExecutor(configuration, runnerSettings, executor, runnerId, state, commandLineType)
    }

    private fun getCustomDelvePath(): String {
        return MirrordPathManager.getBinary("dlv", false)!!
    }
}