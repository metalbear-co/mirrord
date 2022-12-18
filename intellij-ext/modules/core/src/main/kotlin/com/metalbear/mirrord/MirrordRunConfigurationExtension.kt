package com.metalbear.mirrord

import com.goide.execution.GoRunConfigurationBase
import com.intellij.execution.configuration.RunConfigurationExtensionBase
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.RunnerSettings


class MirrordRunConfigurationExtension: RunConfigurationExtensionBase<?<?>> {
    override fun isApplicableFor(configuration: Nothing): Boolean {
        TODO("Not yet implemented")
    }

    override fun isEnabledFor(applicableConfiguration: Nothing, runnerSettings: RunnerSettings?): Boolean {
        TODO("Not yet implemented")
    }

    override fun patchCommandLine(
        configuration: Nothing,
        runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine,
        runnerId: String
    ) {
        TODO("Not yet implemented")
    }
}