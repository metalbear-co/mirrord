package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.util.SystemInfo


class MirrordNpmExecutionListener : ExecutionListener {

	companion object {
		var currentExecutorId: Long = 0
		var originEnv: LinkedHashMap<String, String> = LinkedHashMap()
		var originPackageManagerPackageRef: Any? = null
	}

	private fun detectNpmRunConfiguration(env: ExecutionEnvironment): Boolean {
		return env.runProfile::class.qualifiedName == "com.intellij.lang.javascript.buildTools.npm.rc.NpmRunConfiguration"
	}

	private fun patchNpmEnv(wslDistribution: WSLDistribution?, env: ExecutionEnvironment) {
		try {
			val runSettings = MirrordNpmMutableRunSettings.fromRunProfile(env.runProfile)

			val executablePath = if (SystemInfo.isMac) { runSettings.packageManagerPackagePath } else { null }

			originEnv = LinkedHashMap(runSettings.envs)

			MirrordExecManager.start(wslDistribution, env.project, executablePath)?.let {
				(newEnv, patchedPath) ->

				runSettings.envs = originEnv + newEnv

				patchedPath?.let {
					originPackageManagerPackageRef = runSettings.packageManagerPackageRef
					runSettings.packageManagerPackagePath = it
				}
			}
		} catch (e: Exception) {
			MirrordNotifier.errorNotification("mirrord failed to patch npm run",env.project)
		}
	}

	private fun clearNpmEnv(env: ExecutionEnvironment) {
		val runSettings = MirrordNpmMutableRunSettings.fromRunProfile(env.runProfile)

		try {
			runSettings.envs = originEnv
			originEnv.clear()

			if (SystemInfo.isMac) {
				originPackageManagerPackageRef?.let {
					runSettings.packageManagerPackageRef = it
					originPackageManagerPackageRef = null
				}
			}
		} catch (e: Exception) {
			MirrordNotifier.errorNotification("mirrord failed to clear npm run patch",env.project)
		}
	}

	override fun processStarting(executorId: String, env: ExecutionEnvironment) {
		if (!MirrordExecManager.enabled || !this.detectNpmRunConfiguration(env) || currentExecutorId != 0.toLong()) {
			return super.processStarting(executorId, env)
		}

		currentExecutorId = env.executionId

		val wsl = when (val request = createEnvironmentRequest(env.runProfile, env.project)) {
			is WslTargetEnvironmentRequest -> request.configuration.distribution!!
			else -> null
		}

		patchNpmEnv(wsl, env)

		super.processStartScheduled(executorId, env)
	}

	override fun processStarted(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
		if (currentExecutorId == env.executionId && this.detectNpmRunConfiguration(env)) {
			clearNpmEnv(env)

			currentExecutorId = 0
		}

		super.processStarted(executorId, env, handler)
	}


	override fun processNotStarted(executorId: String, env: ExecutionEnvironment) {
		if (currentExecutorId == env.executionId && this.detectNpmRunConfiguration(env)) {
			clearNpmEnv(env)

			currentExecutorId = 0
		}

		super.processNotStarted(executorId, env)
	}


	override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
		if (currentExecutorId == env.executionId && this.detectNpmRunConfiguration(env)) {
			clearNpmEnv(env)

			currentExecutorId = 0
		}

		return super.processTerminating(executorId, env, handler)
	}
}
