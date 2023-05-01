package com.metalbear.mirrord.products.pycharm

import com.intellij.execution.ExecutionException
import com.intellij.execution.wsl.WslPath
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.SystemInfo
import com.jetbrains.python.run.AbstractPythonRunConfiguration
import com.jetbrains.python.run.PythonExecution
import com.jetbrains.python.run.PythonRunParams
import com.jetbrains.python.run.target.HelpersAwareTargetEnvironmentRequest
import com.jetbrains.python.run.target.PythonCommandLineTargetEnvironmentProvider
import com.metalbear.mirrord.MirrordExecManager


class PythonCommandLineProvider : PythonCommandLineTargetEnvironmentProvider {
    override fun extendTargetEnvironment(
        project: Project,
        helpersAwareTargetRequest: HelpersAwareTargetEnvironmentRequest,
        pythonExecution: PythonExecution,
        runParams: PythonRunParams
    ) {
        if (runParams is AbstractPythonRunConfiguration<*>) {
            try {
                val wsl = helpersAwareTargetRequest.targetEnvironmentRequest.let {
                    if (it is WslTargetEnvironmentRequest) {
                        it.configuration.distribution
                    } else {
                        null
                    }
                }

                MirrordExecManager.start(wsl, project)?.let {
                        env ->
                    for (entry in env.entries.iterator()) {
                        pythonExecution.addEnvironmentVariable(entry.key, entry.value)
                    }
                }

                pythonExecution.addEnvironmentVariable("MIRRORD_DETECT_DEBUGGER_PORT", "pydevd")
            } catch (e: ExecutionException) {
                throw RuntimeException(e)
            }
        }
    }
}