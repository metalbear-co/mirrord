@file:Suppress("DialogTitleCapitalization")

package com.metalbear.mirrord

import com.google.gson.Gson
import com.google.gson.annotations.SerializedName
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.wsl.WSLCommandLineOptions
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.notification.NotificationType
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit


enum class MessageType {
    NewTask,
    FinishedTask,
    Warning
}


// I don't know how to do tags like Rust so this format is for parsing both kind of messages ;_;
data class Message(
    val type: MessageType,
    val name: String,
    val parent: String?,
    val success: Boolean?,
    val message: String?
)

data class Error(
    val message: String,
    val severity: String,
    val causes: List<String>,
    val help: String,
    val labels: List<String>,
    val related: List<String>
)

data class MirrordExecution(
    val environment: MutableMap<String, String>,
    @SerializedName("patched_path")
    val patchedPath: String?
)

/**
 * Interact with mirrord CLI using this API.
 */
object MirrordApi {
    private const val cliBinary = "mirrord"
    const val targetlessTargetName = "No Target (\"targetless\")"
    private val logger = MirrordLogger.logger

    fun cliPath(wslDistribution: WSLDistribution?): String {
        val path = MirrordPathManager.getBinary(cliBinary, true)!!
        wslDistribution?.let {
            return it.getWslPath(path)!!
        }
        return path
    }

    /** run mirrord ls, return list of pods + targetless target. */
    fun listPods(configFile: String?, project: Project?, wslDistribution: WSLDistribution?): List<String>? {
        logger.debug("listing pods")

        val commandLine = GeneralCommandLine(cliPath(wslDistribution), "ls", "-o", "json")
        configFile?.let {
            logger.debug("adding configFile to command line")
            commandLine.addParameter("-f")
            val formattedPath = wslDistribution?.getWslPath(it) ?: it
            commandLine.addParameter(formattedPath)
        }

        wslDistribution?.let {
            logger.debug("patching to use WSL")
            val wslOptions = WSLCommandLineOptions()
            wslOptions.isLaunchWithWslExe = true
            it.patchCommandLine(commandLine, project, wslOptions)
        }

        logger.debug("creating command line and executing $commandLine")

        val process = commandLine.toProcessBuilder()
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        logger.debug("waiting for process to finish")
        process.waitFor(60, TimeUnit.SECONDS)

        logger.debug("process wait finished, reading output")

        // failure -> null
        // success -> empty -> targetless, else -> list of pods
        val gson = Gson()
        if (process.exitValue() != 0) {
            val processStdError = process.errorStream.bufferedReader().readText()
            if (processStdError.startsWith("Error: ")) {
                val trimmedError = processStdError.removePrefix("Error: ")
                val error = gson.fromJson(trimmedError, Error::class.java)
                MirrordNotifier.errorNotification(error.message, project)
                MirrordNotifier.notify(error.help, NotificationType.INFORMATION, project)
                return null
            }
            logger.debug("mirrord ls failed: $processStdError")
        }

        val data = process.inputStream.bufferedReader().readText()
        logger.debug("parsing %s".format(data))
        val pods = gson.fromJson(data, Array<String>::class.java).toMutableList()

        if (pods.isEmpty()) {
            MirrordNotifier.notify(
                "No mirrord target available in the configured namespace. " +
                        "You can run targetless, or set a different target namespace or kubeconfig in the mirrord configuration file.",
                NotificationType.INFORMATION,
                project,
            )
        }

        pods.add(targetlessTargetName)
        return pods
    }

    fun exec(
        target: String?,
        configFile: String?,
        executable: String?,
        project: Project?,
        wslDistribution: WSLDistribution?
    ): MutableMap<String, String>? {
        val commandLine = GeneralCommandLine(cliPath(wslDistribution), "ext").apply {
            target?.let {
                addParameter("-t")
                addParameter(it)
            }

            configFile?.let {
                val formattedPath = wslDistribution?.getWslPath(it) ?: it
                addParameter("-f")
                addParameter(formattedPath)
            }
            executable?.let {
              commandLine.addParameter("-e")
              commandLine.addParameter(executable)
            }
            
            wslDistribution?.let {
                val wslOptions = WSLCommandLineOptions().apply {
                    isLaunchWithWslExe = true
                }
                it.patchCommandLine(this, project, wslOptions)
            }

        }

        logger.info("running mirrord with following command line: ${commandLine.commandLineString}")

        val process = commandLine.toProcessBuilder()
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        logger.debug("waiting for process to finish")
        val exitStatus = process.waitFor(15, TimeUnit.SECONDS)

        if (!exitStatus) {
            MirrordNotifier.errorNotification("mirrord failed to start", project)
            throw Error("mirrord failed to start")
        }

        val bufferedReader = process.inputStream.reader().buffered()
        val gson = Gson()

        val environment = CompletableFuture<MirrordExecution?>()

        val mirrordProgressTask = object : Task.Backgroundable(project, "mirrord", true) {
            override fun run(indicator: ProgressIndicator) {
                indicator.text = "mirrord is starting..."
                for (line in bufferedReader.lines()) {
                    val message = gson.fromJson(line, Message::class.java)
                    when {
                        message.name == "mirrord preparing to launch" && message.type == MessageType.FinishedTask -> {
                            val success = message.success ?: throw Error("Invalid message")
                            if (success) {
                                val innerMessage = message.message ?: throw Error("Invalid inner message")
                                val executionInfo = gson.fromJson(innerMessage, MirrordExecution::class.java)
                                indicator.text = "mirrord is running"
                                environment.complete(executionInfo)
                                return
                            }
                        }

                        message.type == MessageType.Warning -> {
                            message.message?.let { MirrordNotifier.notify(it, NotificationType.WARNING, project) }
                        }

                        else -> {
                            var displayMessage = message.name
                            message.message?.let {
                                displayMessage += ": $it"
                            }
                            indicator.text = displayMessage
                        }
                    }
                }
                environment.cancel(true)
            }

            override fun onSuccess() {
                if (!environment.isCancelled) {
                    MirrordNotifier.notify("mirrord started!", NotificationType.INFORMATION, project)
                }
                super.onSuccess()
            }

            override fun onCancel() {
                process.destroy()
                MirrordNotifier.notify("mirrord was cancelled", NotificationType.WARNING, project)
                super.onCancel()
            }
        }

        ProgressManager.getInstance().run(mirrordProgressTask)

        try {
            return environment.get(30, TimeUnit.SECONDS)
        } catch (e: Exception) {
            MirrordNotifier.errorNotification("mirrord failed to fetch the env", project)
        }

        val processStdError = process.errorStream.reader().readText()
        if (processStdError.startsWith("Error: ")) {
            val trimmedError = processStdError.removePrefix("Error: ")
            val error = gson.fromJson(trimmedError, Error::class.java)
            MirrordNotifier.errorNotification(error.message, project)
            MirrordNotifier.notify(error.help, NotificationType.INFORMATION, project)
            return null
        }
        logger.error("mirrord stderr: $processStdError")
        throw Error("mirrord failed to start")
    }

}