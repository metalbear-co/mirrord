package com.metalbear.mirrord

import com.google.gson.Gson
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.wsl.WSLCommandLineOptions
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.idea.LoggerFactory
import com.intellij.notification.NotificationType
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.diagnostic.thisLogger
import com.intellij.openapi.project.Project
import com.intellij.util.io.runClosingOnFailure
import java.util.concurrent.TimeUnit


enum class MessageType {
    NewTask,
    FinishedTask
}


// I don't know how to do tags like Rust so this format is for parsing both kind of messages ;_;
data class Message (
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
    private val logger = Logger.getInstance("mirrord")
    private fun cliPath(wslDistribution: WSLDistribution?): String {
        val path = MirrordPathManager.getBinary(cliBinary, true)!!
        wslDistribution?.let {
            return it.getWslPath(path)!!
        }
        return path
    }

    fun listPods(namespace: String?, project: Project?, wslDistribution: WSLDistribution?): List<String> {
        var commandLine = GeneralCommandLine(cliPath(wslDistribution), "ls", "-o", "json")
        namespace?.let {
            commandLine.addParameter("-n")
            commandLine.addParameter(namespace)
        }


        wslDistribution?.let {
            val wslOptions = WSLCommandLineOptions()
            wslOptions.isLaunchWithWslExe = true
            it.patchCommandLine(commandLine, project, wslOptions)
        }

        val process = commandLine.toProcessBuilder()
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        process.waitFor(60, TimeUnit.SECONDS)

        val data = process.inputStream.bufferedReader().readText();
        val gson = Gson();
        return gson.fromJson(data, Array<String>::class.java).asList()

    }

    fun exec(target: String?, configFile: String?, project: Project?, wslDistribution: WSLDistribution?): MutableMap<String, String> {
        var commandLine = GeneralCommandLine(cliPath(wslDistribution), "ext")

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
        val gson = Gson();
        for (line in bufferedReader.lines()) {
            logger.info("mirrord output: %s".format(line))
            val message = gson.fromJson(line, Message::class.java)
            // See if it's the final message
            if (message.name == "mirrord preparing to launch"
                && message.type == MessageType.FinishedTask) {
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

            var displayMessage = message.name
            message.message?.let {
                displayMessage += ": $it"
            }

            MirrordNotifier.progress(displayMessage, project)
        }


        logger.error("mirrord stderr: %s".format(process.errorStream.reader().readText()))
        MirrordNotifier.errorNotification("mirrord failed to launch", project)
        throw Error("failed launch")

    }
}