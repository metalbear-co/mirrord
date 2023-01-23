package com.metalbear.mirrord.products.rust

import com.intellij.execution.Executor
import com.intellij.execution.configurations.CommandLineState
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.diagnostic.Logger
import com.metalbear.mirrord.MirrordExecManager
import org.rust.cargo.runconfig.CargoCommandConfigurationExtension
import org.rust.cargo.runconfig.ConfigurationExtensionContext
import org.rust.cargo.runconfig.command.CargoCommandConfiguration

class RustRunConfigurationExtension :  CargoCommandConfigurationExtension() {
    override fun attachToProcess(
        configuration: CargoCommandConfiguration,
        handler: ProcessHandler,
        environment: ExecutionEnvironment,
        context: ConfigurationExtensionContext
    ) {
        Logger.getInstance("mirrord").debug("Here")
        // nothing we need to do here
    }

    override fun isApplicableFor(configuration: CargoCommandConfiguration): Boolean {
        Logger.getInstance("mirrord").debug("here")
        return true
    }

    override fun isEnabledFor(
        applicableConfiguration: CargoCommandConfiguration,
        runnerSettings: RunnerSettings?
    ): Boolean {
        Logger.getInstance("mirrord").debug("here")
        return true
    }


    override fun patchCommandLine(
        configuration: CargoCommandConfiguration,
        environment: ExecutionEnvironment,
        cmdLine: GeneralCommandLine,
        context: ConfigurationExtensionContext
    ) {
        Logger.getInstance("mirrord").debug("here")
        val wsl = when (val request = environment.targetEnvironmentRequest) {
            is WslTargetEnvironmentRequest -> request.configuration.distribution!!
            else -> null
        }

        val project = configuration.project
        val currentEnv = cmdLine.environment

        MirrordExecManager.start(wsl, project)?.let { env ->
            for (entry in env.entries.iterator()) {
                currentEnv[entry.key] = entry.value
            }
        }
    }

    override fun patchCommandLine(
        configuration: CargoCommandConfiguration,
        runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine,
        runnerId: String
    ) {
        Logger.getInstance("mirrord").debug("here")
        super.patchCommandLine(configuration, runnerSettings, cmdLine, runnerId)
    }

    override fun patchCommandLine(
        configuration: CargoCommandConfiguration,
        runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine,
        runnerId: String,
        executor: Executor
    ) {
        Logger.getInstance("mirrord").debug("here")
        super.patchCommandLine(configuration, runnerSettings, cmdLine, runnerId, executor)
    }

    override fun patchCommandLineState(
        configuration: CargoCommandConfiguration,
        environment: ExecutionEnvironment,
        state: CommandLineState,
        context: ConfigurationExtensionContext
    ) {
        Logger.getInstance("mirrord").debug("here")
        super.patchCommandLineState(configuration, environment, state, context)
    }
}