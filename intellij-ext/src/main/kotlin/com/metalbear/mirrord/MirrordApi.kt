package com.metalbear.mirrord

import com.google.gson.Gson
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.wsl.WSLCommandLineOptions
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.progress.Task.Backgroundable
import com.intellij.openapi.project.Project
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


/**
 * Interact with mirrord CLI using this API.
 */
object MirrordApi {
    private const val cliBinary = "mirrord"
    private fun cliPath(): String = MirrordPathManager.getBinary(cliBinary, true)!!

    fun listPods(namespace: String?, project: Project?, wslDistribution: WSLDistribution?): List<String> {
        var commandLine = GeneralCommandLine(cliPath(), "ls", "-o", "json")
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

    fun exec(target: String?, configFile: String?, project: Project?, wslDistribution: WSLDistribution?): Map<String, String> {
        var commandLine = GeneralCommandLine(cliPath(), "ext")

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


        ProgressManager.getInstance().run(object : Backgroundable(project, "Title") {
            override fun run(progressIndicator: ProgressIndicator) {

                // start your process

                // Set the progress bar percentage and text
                progressIndicator.fraction = 0.10
                progressIndicator.text = "90% to finish"
                Thread.sleep(10000)


                // 50% done
                progressIndicator.fraction = 0.50
                progressIndicator.text = "50% to finish"
                Thread.sleep(10000)

                // Finished
                progressIndicator.fraction = 1.0
                progressIndicator.text = "finished"
            }
        })



        val process = commandLine.toProcessBuilder()
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        val bufferedReader = process.inputStream.reader().buffered()
        val gson = Gson();
        for (line in bufferedReader.lines()) {
            val message = gson.fromJson(line, Message::class.java)

        }




        val data = process.inputStream.bufferedReader().readText();
        return mapOf("2" to "b")

//        return mapOf((1,1))
//        return gson.fromJson(data, Array<String>::class.java).asList()
    }
}