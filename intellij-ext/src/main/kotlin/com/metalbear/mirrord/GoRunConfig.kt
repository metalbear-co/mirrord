package com.metalbear.mirrord

import com.goide.execution.GoRunConfigurationBase
import com.goide.execution.GoRunningState
import com.goide.execution.extension.GoRunConfigurationExtension
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.TargetedCommandLineBuilder

class GoRunConfig : GoRunConfigurationExtension() {
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
            val (ldPreloadPath, dylibPath, defaultMirrordAgentLog, rustLog, invalidCertificates, ephemeralContainers) = MirrordDefaultData()

            cmdLine.addEnvironmentVariable("DYLD_INSERT_LIBRARIES", dylibPath)
            cmdLine.addEnvironmentVariable("MIRRORD_DYLIB_PATH", "/Users/mehularora/Documents/mirrord/target/debug/libmirrord_layer.dylib")
            cmdLine.addEnvironmentVariable("MIRRORD_ACCEPT_INVALID_CERTIFICATES", "true")
            cmdLine.addEnvironmentVariable("RUST_LOG", "DEBUG")
            cmdLine.addEnvironmentVariable(
                "MIRRORD_AGENT_IMPERSONATED_POD_NAME",
                "metalbear-deployment-85c754c75f-qnc8t"
            )
        }
        super.patchCommandLine(configuration, runnerSettings, cmdLine, runnerId, state, commandLineType)
    }
}