package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.configuration.EnvironmentVariablesData
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.notification.*


class MirrordExecutionListener : ExecutionListener {

	companion object {
		var mirrordEnv: LinkedHashMap<String, String> = LinkedHashMap()
		var originPackageManagerPackageRef: Any? = null
	}

	private fun detectNpmRunConfiguration(env: ExecutionEnvironment): Boolean {
		return env.runProfile::class.qualifiedName == "com.intellij.lang.javascript.buildTools.npm.rc.NpmRunConfiguration"
	}

	private fun patchNpmEnv(env: ExecutionEnvironment) {
		val runProfile = env.runProfile

		try {
			val getRunSettings = runProfile.javaClass.getMethod("getRunSettings")
			val runSettings = getRunSettings.invoke(runProfile)

			val getEnvData = runSettings.javaClass.getMethod("getEnvData")
			val envData = getEnvData.invoke(runSettings) as EnvironmentVariablesData

			val newEnvData = envData.with(envData.envs + mirrordEnv)

			val toBuilder = runSettings.javaClass.getMethod("toBuilder")
			val builder = toBuilder.invoke(runSettings)

			originPackageManagerPackageRef = runSettings.javaClass.getMethod("getPackageManagerPackageRef").invoke(runSettings)

			var createPackageManagerPackageRef: java.lang.reflect.Method? = null

			for (i in originPackageManagerPackageRef!!.javaClass.methods.indices) {
				if (originPackageManagerPackageRef!!.javaClass.methods[i].name == "create" && originPackageManagerPackageRef!!.javaClass.methods[i].parameterTypes[0].name != "java.lang.String") {
					createPackageManagerPackageRef = originPackageManagerPackageRef!!.javaClass.methods[i];
				}
			}

			val packageManagerPackageRef = createPackageManagerPackageRef!!.invoke(null, createPackageManagerPackageRef!!.parameterTypes[0].getConstructor(Class.forName("java.lang.String")).newInstance(MirrordApi.cliPath(null)))

//			val packageManagerPackageRef = originPackageManagerPackageRef!!.javaClass.getMethod("create", Class.forName("java.lang.String")).invoke(null, MirrordApi.cliPath(null))
			builder.javaClass.getMethod("setPackageManagerPackageRef", packageManagerPackageRef.javaClass).invoke(builder, packageManagerPackageRef)

			val setEnvData = builder.javaClass.getMethod("setEnvData", newEnvData.javaClass)
			setEnvData.invoke(builder, newEnvData)

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

			builder.javaClass.getMethod("setPackageManagerPackageRef", originPackageManagerPackageRef!!.javaClass).invoke(builder, originPackageManagerPackageRef)

            originPackageManagerPackageRef = null

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

	override fun processStartScheduled(executorId: String, env: ExecutionEnvironment) {
		if (!MirrordExecManager.enabled || !this.detectNpmRunConfiguration(env)) {
			return super.processStartScheduled(executorId, env)
		}

		val runProfile = env.runProfile

		MirrordExecManager.start(null, env.project)?.let {
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

		this.patchNpmEnv(env)

		return super.processStartScheduled(executorId, env)
	}

	override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
		if (this.detectNpmRunConfiguration(env)) {
			this.clearNpmEnv(env)
		}
	}
}
