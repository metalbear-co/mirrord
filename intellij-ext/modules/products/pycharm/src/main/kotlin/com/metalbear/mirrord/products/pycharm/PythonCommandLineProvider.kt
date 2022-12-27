package com.metalbear.mirrord.products.pycharm

import com.intellij.execution.ExecutionException
import com.intellij.openapi.project.Project
import com.jetbrains.python.run.AbstractPythonRunConfiguration
import com.jetbrains.python.run.PythonExecution
import com.jetbrains.python.run.PythonRunParams
import com.jetbrains.python.run.target.HelpersAwareTargetEnvironmentRequest
import com.jetbrains.python.run.target.PythonCommandLineTargetEnvironmentProvider
//import net.ashald.envfile.platform.EnvFileEnvironmentVariables
//import net.ashald.envfile.platform.ui.EnvFileConfigurationEditor


class EnvFileProvider : PythonCommandLineTargetEnvironmentProvider {
    override fun extendTargetEnvironment(
        project: Project,
        helpersAwareTargetEnvironmentRequest: HelpersAwareTargetEnvironmentRequest,
        pythonExecution: PythonExecution,
        pythonRunParams: PythonRunParams
    ) {
        if (pythonRunParams is AbstractPythonRunConfiguration<*>) { // Copied from EnvFile, not sure it's necessary
            try {

            } catch (e: ExecutionException) {
                throw RuntimeException(e)
            }
        }
    }
}