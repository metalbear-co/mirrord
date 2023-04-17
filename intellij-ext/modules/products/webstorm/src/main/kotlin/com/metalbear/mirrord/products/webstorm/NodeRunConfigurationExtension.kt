package com.metalbear.mirrord.products.webstorm

import com.intellij.execution.Executor
//import com.intellij.execution.RunConfigurationExtension
//import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.javascript.nodejs.execution.runConfiguration.AbstractNodeRunConfigurationExtension;
import com.intellij.openapi.externalSystem.service.execution.ExternalSystemRunConfiguration
import com.metalbear.mirrord.MirrordExecManager
import com.metalbear.mirrord.MirrordLogger
import com.jetbrains.nodejs.run.NodeJsRunConfiguration
import java.lang.Override

import java.util.concurrent.ExecutionException





class NodeRunConfigurationExtension: AbstractNodeRunConfigurationExtension() {
    override fun readExternal(runConfiguration: AbstractNodeTargetRunProfile?, element: Element?) {
        RunConfigSettingsEditor.readExternal(runConfiguration, element)
    }

    protected fun writeExternal(runConfiguration: AbstractNodeTargetRunProfile?, element: Element?) {
        RunConfigSettingsEditor.writeExternal(runConfiguration, element)
    }

    override fun <P : AbstractNodeTargetRunProfile> createEditor(configuration: P): SettingsEditor<P> {
        return RunConfigSettingsEditor(configuration)
    }

    fun isApplicableFor(configuration: AbstractNodeTargetRunProfile?): Boolean {
        return true
    }

    @kotlin.Throws(java.util.concurrent.ExecutionException::class)
    protected fun patchCommandLine(
        configuration: AbstractNodeTargetRunProfile,
        @Nullable runnerSettings: RunnerSettings?,
        cmdLine: GeneralCommandLine?,
        runnerId: String?,
        executor: Executor?
    ) {
        configuration.getSelectedOptions()
    }

    protected fun extendCreatedConfiguration(
        configuration: AbstractNodeTargetRunProfile?,
        location: Location?
    ) {
        super.extendCreatedConfiguration(configuration, location)
    }

    @Nullable
    fun getEditorTitle(): String? {
        return RunConfigSettingsEditor.getEditorTitle()
    }

    @Nullable
    @kotlin.Throws(java.util.concurrent.ExecutionException::class)
    fun createLaunchSession(
        configuration: AbstractNodeTargetRunProfile,
        environment: ExecutionEnvironment?
    ): NodeRunConfigurationLaunchSession? {
        val config: NodeJsRunConfiguration = configuration as NodeJsRunConfiguration
        val newEnvs: Map<String, String> = RunConfigSettingsEditor
            .collectEnv(configuration, config.getWorkingDirectory(), config.getEnvs())
        config.setEnvs(newEnvs)
        return null
    }
//    override fun isApplicableFor(configuration: RunConfigurationBase<*>): Boolean {
//        return true
//    }
//
//
//    override fun isEnabledFor(
//        applicableConfiguration: RunConfigurationBase<*>,
//        runnerSettings: RunnerSettings?
//    ): Boolean {
//        return true
//    }
//
//
//    private fun < T: RunConfigurationBase<*>> patchEnv (configuration: T, params: JavaParameters) {
//        MirrordLogger.logger.debug("Check if relevant")
//        if (configuration.name.startsWith("Build ")) {
//            MirrordLogger.logger.info("Configuration name %s ignored".format(configuration.name))
//            return
//        }
//        MirrordLogger.logger.debug("wsl check")
//        val wsl = when (val request = createEnvironmentRequest(configuration, configuration.project)) {
//            is WslTargetEnvironmentRequest -> request.configuration.distribution!!
//            else -> null
//        }
//
//        MirrordLogger.logger.debug("getting env")
//        val project = configuration.project
//        val currentEnv = HashMap<String, String>()
//        currentEnv.putAll(params.env)
//
//        val mirrordEnv = HashMap<String, String>()
//        MirrordLogger.logger.debug("calling start")
//        MirrordExecManager.start(wsl, project)?.let {
//                env ->
//            for (entry in env.entries.iterator()) {
//                mirrordEnv[entry.key] =  entry.value
//            }
//        }
//
//        params.env = currentEnv + mirrordEnv
//
//        // Gradle support (and external system configuration)
//        if (configuration is ExternalSystemRunConfiguration) {
//            val ext = configuration as ExternalSystemRunConfiguration
//            val newEnv = ext.settings.env + mirrordEnv
//            ext.settings.env = newEnv
//        }
//        MirrordLogger.logger.debug("setting env and finishing")
//    }
//    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
//        configuration: T,
//        params: JavaParameters,
//        runnerSettings: RunnerSettings?,
//        executor: Executor
//    ) {
//        MirrordLogger.logger.debug("updateJavaParameters called")
//        patchEnv(configuration, params)
//    }
//
//    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
//        configuration: T,
//        params: JavaParameters,
//        runnerSettings: RunnerSettings?
//    ) {
//        MirrordLogger.logger.debug("updateJavaParameters (with less parameters) called")
//        patchEnv(configuration, params)
//    }
}