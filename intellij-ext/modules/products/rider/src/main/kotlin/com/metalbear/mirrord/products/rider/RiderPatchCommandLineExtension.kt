package com.metalbear.mirrord.products.rider

import com.intellij.execution.RunManager
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.ProcessListener
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.project.Project
import com.jetbrains.rd.util.lifetime.Lifetime
import com.jetbrains.rider.run.PatchCommandLineExtension
import com.jetbrains.rider.run.WorkerRunInfo
import com.jetbrains.rider.runtime.DotNetRuntime
import com.metalbear.mirrord.MirrordExecManager
import org.jetbrains.concurrency.Promise
import org.jetbrains.concurrency.resolvedPromise

class RiderPatchCommandLineExtension : PatchCommandLineExtension {
    private fun patchCommandLine(commandLine: GeneralCommandLine, project: Project) {
        val wsl = RunManager.getInstance(project).selectedConfiguration?.configuration?.let {
            when (val request = createEnvironmentRequest(it, project)) {
                is WslTargetEnvironmentRequest -> request.configuration.distribution!!
                else -> null
            }
        }


        MirrordExecManager.start(wsl, project)?.let {
            env ->
            for (entry in env.entries.iterator()) {
                commandLine.withEnvironment(entry.key, entry.value)
            }
        }
    }

    override fun patchDebugCommandLine(
            lifetime: Lifetime,
            workerRunInfo: WorkerRunInfo,
            project: Project
    ): Promise<WorkerRunInfo> {
        patchCommandLine(workerRunInfo.commandLine, project)
        return resolvedPromise(workerRunInfo)
    }

    override fun patchRunCommandLine(
            commandLine: GeneralCommandLine,
            dotNetRuntime: DotNetRuntime,
            project: Project
    ): ProcessListener? {
        patchCommandLine(commandLine, project)
        return null
    }
}