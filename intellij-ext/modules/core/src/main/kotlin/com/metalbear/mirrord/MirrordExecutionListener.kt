package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.configuration.EnvironmentVariablesData
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.notification.*
import com.intellij.openapi.util.SystemInfo
import java.util.concurrent.TimeUnit


class MirrordExecutionListener : ExecutionListener {

	companion object {
		var mirrordEnv: LinkedHashMap<String, String> = LinkedHashMap()
		var originInterpreterRef: Any? = null
		var originPackageManagerPackageRef: Any? = null
	}

	private fun detectNpmRunConfiguration(env: ExecutionEnvironment): Boolean {
		return env.runProfile::class.qualifiedName == "com.intellij.lang.javascript.buildTools.npm.rc.NpmRunConfiguration"
	}

	private fun patchSip(wslDistribution: WSLDistribution?, executablePath: String): String {
		val commandLine = GeneralCommandLine(MirrordApi.cliPath(wslDistribution), "sip-patch", executablePath)

		val process = commandLine.toProcessBuilder()
				.redirectOutput(ProcessBuilder.Redirect.PIPE)
				.redirectError(ProcessBuilder.Redirect.PIPE)
				.start()

		process.waitFor(5, TimeUnit.SECONDS)

		if (process.exitValue() != 0) {
			val data = process.errorStream.bufferedReader().readText()
			MirrordLogger.logger.debug("mirrord sip-patch failed: %s".format(data))

			return executablePath
		}

		return process.inputStream.bufferedReader().readText().trim()
	}

	private fun patchNpmEnv(env: ExecutionEnvironment, wslDistribution: WSLDistribution?) {
		val project = env.project
		val runProfile = env.runProfile

		try {
			val getRunSettings = runProfile.javaClass.getMethod("getRunSettings")
			val runSettings = getRunSettings.invoke(runProfile)

			val myEnvData = runSettings.javaClass.getDeclaredField("myEnvData")
			myEnvData.isAccessible = true

			val envData = myEnvData.get(runSettings) as EnvironmentVariablesData
			val newEnvData = envData.with(envData.envs + mirrordEnv)

			myEnvData.set(runSettings, newEnvData)

			if (SystemInfo.isMac) {
				val myInterpreterRef = runSettings.javaClass.getDeclaredField("myInterpreterRef")
				myInterpreterRef.isAccessible = true

				originInterpreterRef = myInterpreterRef.get(runSettings)

				val resolveInterpreter = originInterpreterRef!!.javaClass.methods.find { k -> k.name == "resolve" }
				val interpreter = resolveInterpreter!!.invoke(originInterpreterRef, project)

				val patchedInterpreterPath = patchSip(wslDistribution, interpreter.toString())

				val patchedInterpreter = interpreter.javaClass.getConstructor(Class.forName("java.lang.String")).newInstance(patchedInterpreterPath)

				val toInterpreterRef =  patchedInterpreter.javaClass.getMethod("toRef")
				val patchedInterpreterRef = toInterpreterRef.invoke(patchedInterpreter)

				myInterpreterRef.set(runSettings, patchedInterpreterRef)

				val myPackageManagerPackageRef = runSettings.javaClass.getDeclaredField("myPackageManagerPackageRef")
				myPackageManagerPackageRef.isAccessible = true

				originPackageManagerPackageRef = myPackageManagerPackageRef.get(runSettings)

				val packageManager = originPackageManagerPackageRef!!.javaClass.getMethod("getConstantPackage").invoke(originPackageManagerPackageRef)

				if (packageManager != null) {
					val getSystemIndependentPath = packageManager.javaClass.getMethod("getSystemIndependentPath")
					val packageManagerPath = getSystemIndependentPath.invoke(packageManager) as String

					val patchedPath = patchSip(wslDistribution, packageManagerPath)

					val patchedPackageManager = packageManager.javaClass.getConstructor(Class.forName("java.lang.String")).newInstance(patchedPath)

					val createPackageManagerPackageRef = originPackageManagerPackageRef!!.javaClass.methods.find { m -> m.name == "create" && m.parameterTypes[0].name != "java.lang.String" }

					val packageManagerPackageRef = createPackageManagerPackageRef!!.invoke(null, patchedPackageManager)
					myPackageManagerPackageRef.set(runSettings, packageManagerPackageRef)
				}
			}
		} catch (e: Exception) {
			MirrordNotifier.notify(
					"${runProfile::class.qualifiedName}: $e",
					NotificationType.ERROR,
					env.project
			)
		}
	}

	private fun clearNpmEnv(env: ExecutionEnvironment) {
		val runProfile = env.runProfile

		try {
			val getRunSettings = runProfile.javaClass.getMethod("getRunSettings")
			val runSettings = getRunSettings.invoke(runProfile)

			val getEnvData = runSettings.javaClass.getMethod("getEnvData")
			val envData = getEnvData.invoke(runSettings) as EnvironmentVariablesData

			val envMap = LinkedHashMap(envData.envs)

			for (key in mirrordEnv.keys) {
				if (envMap.containsKey(key)) {
					envMap.remove(key)
				}
			}

			val newEnvData = envData.with(envMap)

			val toBuilder = runSettings.javaClass.getMethod("toBuilder")
			val builder = toBuilder.invoke(runSettings)

			val setEnvData = builder.javaClass.getMethod("setEnvData", newEnvData.javaClass)
			setEnvData.invoke(builder, newEnvData)

			if (SystemInfo.isMac) {
				builder.javaClass.getMethod("setPackageManagerPackageRef", originPackageManagerPackageRef!!.javaClass).invoke(builder, originPackageManagerPackageRef)
				originPackageManagerPackageRef = null
			}

			val build = builder.javaClass.getMethod("build")
			val newRunSettings = build.invoke(builder)

			val setRunSettings = runProfile.javaClass.getMethod("setRunSettings", newRunSettings.javaClass)
			setRunSettings.invoke(runProfile, newRunSettings)
		} catch (e: Exception) {
			MirrordNotifier.notify(
					"${runProfile::class.qualifiedName}: $e",
					NotificationType.ERROR,
					env.project
			)
		}
	}

	override fun processStarting(executorId: String, env: ExecutionEnvironment) {
		if (!MirrordExecManager.enabled || !this.detectNpmRunConfiguration(env)) {
			return super.processStarting(executorId, env)
		}

		val runProfile = env.runProfile
		val project = env.project

		val wsl = when (val request = createEnvironmentRequest(runProfile, project)) {
			is WslTargetEnvironmentRequest -> request.configuration.distribution!!
			else -> null
		}

		MirrordExecManager.start(wsl, env.project)?.let {
			newEnv ->
			for (entry in newEnv.entries.iterator()) {
				mirrordEnv[entry.key] = entry.value
			}
		}

		MirrordNotifier.notify(
				"${runProfile::class.qualifiedName}: $mirrordEnv",
				NotificationType.INFORMATION,
				env.project
		)

		patchNpmEnv(env, wsl)

		super.processStartScheduled(executorId, env)
	}


	override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
		if (this.detectNpmRunConfiguration(env)) {
			clearNpmEnv(env)
		}
	}
}
