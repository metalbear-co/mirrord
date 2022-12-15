package com.metalbear.mirrord

import com.google.gson.Gson
import java.util.concurrent.TimeUnit

/**
 * Interact with mirrord CLI using this API.
 */
object MirrordApi {
    private const val cliBinary = "mirrord"
    private fun cliPath(): String = MirrordPathManager.getBinary(cliBinary, true)!!

    fun listPods(namespace: String?): List<String> {
        val commandLine = mutableListOf<String>(cliPath(), "ls", "-o", "json")
        if (namespace != null) {
            commandLine.add("-n")
            commandLine.add(namespace)
        }

        val process = ProcessBuilder(commandLine)
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.PIPE)
            .start()

        process.waitFor(60, TimeUnit.SECONDS)

        val data = process.inputStream.bufferedReader().readText();
        val gson = Gson();
        return gson.fromJson(data, Array<String>::class.java).asList()

    }
}