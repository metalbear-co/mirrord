package com.metalbear.mirrord

import com.google.gson.Gson
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.wsl.WSLCommandLineOptions
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.notification.NotificationType
import com.intellij.openapi.project.Project
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

data class MirrordExecution(
    val environment: MutableMap<String, String>
)

/**
 * Interact with mirrord CLI using this API.
 */
object MirrordApi {
    private const val cliBinary = "mirrord"
    private val logger = MirrordLogger.logger
    private fun cliPath(wslDistribution: WSLDistribution?): String {
        val path = MirrordPathManager.getBinary(cliBinary, true)!!
        wslDistribution?.let {
            return it.getWslPath(path)!!
        }
        return path
    }

    fun listPods(configFile: String?, project: Project?, wslDistribution: WSLDistribution?): List<String>? {
        MirrordLogger.logger.debug("listing pods")
        val commandLine = GeneralCommandLine(cliPath(wslDistribution), "ls", "-o", "json")
        configFile?.let {
            MirrordLogger.logger.debug("adding configFile to command line")
            commandLine.addParameter("-f")
            val formattedPath = wslDistribution?.getWslPath(it) ?: it
            commandLine.addParameter(formattedPath)
        }


        wslDistribution?.let {
            MirrordLogger.logger.debug("patching to use WSL")
            val wslOptions = WSLCommandLineOptions()
            wslOptions.isLaunchWithWslExe = true
            it.patchCommandLine(commandLine, project, wslOptions)
        }

        MirrordLogger.logger.debug("creating command line and executing %s".format(commandLine.toString()))
        val process = commandLine.toProcessBuilder()
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        MirrordLogger.logger.debug("waiting for process to finish")
        process.waitFor(60, TimeUnit.SECONDS)

        if (process.exitValue() != 0) {
            MirrordNotifier.errorNotification("mirrord failed to list available targets", project)
            val data = process.errorStream.bufferedReader().readText()
            MirrordLogger.logger.debug("mirrord ls failed: %s".format(data))
            return null
        }

        MirrordLogger.logger.debug("process wait finished, reading output")
        val data = process.inputStream.bufferedReader().readText()
        val gson = Gson()
        MirrordLogger.logger.debug("parsing %s".format(data))
        return gson.fromJson(data, Array<String>::class.java).asList()

    }

    fun exec(
        target: String?,
        configFile: String?,
        project: Project?,
        wslDistribution: WSLDistribution?
    ): MutableMap<String, String> {
        val commandLine = GeneralCommandLine(cliPath(wslDistribution), "ext")

        target?.let {
            commandLine.addParameter("-t")
            commandLine.addParameter(target)
        }

        configFile?.let {
            // Format according to wslDistribution if exists
            val formattedPath = wslDistribution?.getWslPath(it) ?: it
            commandLine.addParameter("-f")
            commandLine.addParameter(formattedPath)
        }

        wslDistribution?.let {
            val wslOptions = WSLCommandLineOptions()
            wslOptions.isLaunchWithWslExe = true
            it.patchCommandLine(commandLine, project, wslOptions)
        }

        logger.info("running mirrord with following commandline: %s".format(commandLine.commandLineString))

        val process = commandLine.toProcessBuilder()
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        val bufferedReader = process.inputStream.reader().buffered()
        val gson = Gson()

        for (line in bufferedReader.lines()) {
            val message = gson.fromJson(line, Message::class.java)
            // See if it's the final message
            if (message.name == "mirrord preparing to launch"
                && message.type == MessageType.FinishedTask
            ) {
                val success = message.success ?: throw Error("Invalid message")
                if (success) {
                    val innerMessage = message.message ?: throw Error("Invalid inner message")
                    val executionInfo = gson.fromJson(innerMessage, MirrordExecution::class.java)
                    MirrordNotifier.progress("mirrord started!", project)
                    return executionInfo.environment
                } else {
                    MirrordNotifier.errorNotification("mirrord failed to launch", project)
                }
            }
            if (message.type == MessageType.Warning) {
                message.message?.let { MirrordNotifier.notify(it, NotificationType.WARNING, project) }
            } else {
                var displayMessage = message.name
                message.message?.let {
                    displayMessage += ": $it"
                }
                MirrordNotifier.progress(displayMessage, project)
            }
        }

        logger.error("mirrord stderr: %s".format(process.errorStream.reader().readText()))
        MirrordNotifier.errorNotification("mirrord failed to launch", project)
        throw Error("failed launch")
    }
}