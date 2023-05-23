package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.target.createEnvironmentRequest
import com.intellij.execution.wsl.WSLDistribution
import com.intellij.execution.wsl.target.WslTargetEnvironmentRequest
import com.intellij.openapi.util.SystemInfo


data class RunConfigGuard(val executionId: Long) {
	var originEnv: Map<String, String> = LinkedHashMap()
	var originPackageManagerPackageRef: Any? = null
}

class MirrordNpmExecutionListener : ExecutionListener {

	companion object {
		val executions: MutableMap<Long, RunConfigGuard> = LinkedHashMap()
	}

	private fun detectNpmRunConfiguration(env: ExecutionEnvironment): Boolean {
		return env.runProfile::class.qualifiedName == "com.intellij.lang.javascript.buildTools.npm.rc.NpmRunConfiguration"
	}

	private fun patchNpmEnv(wslDistribution: WSLDistribution?, env: ExecutionEnvironment) {
		val executionGuard = executions[env.executionId]!!

		try {
			val runSettings = MirrordNpmMutableRunSettings.fromRunProfile(env.project, env.runProfile)

			val executablePath = if (SystemInfo.isMac) { runSettings.packageManagerPackagePath } else { null }

			executionGuard.originEnv = LinkedHashMap(runSettings.envs)

			MirrordExecManager.start(wslDistribution, env.project, executablePath)?.let {
				(newEnv, patchedPath) ->

				runSettings.envs = executionGuard.originEnv + newEnv

				patchedPath?.let {
					executionGuard.originPackageManagerPackageRef = runSettings.packageManagerPackageRef
					runSettings.packageManagerPackagePath = it
				}
			}
		} catch (e: Exception) {
			MirrordLogger.logger.error("mirrord failed to patch npm run: $e")
			MirrordNotifier.errorNotification("mirrord failed to patch npm run",env.project)
		}
	}

	private fun clearNpmEnv(env: ExecutionEnvironment) {
		val executionGuard = executions[env.executionId]!!
		val runSettings = MirrordNpmMutableRunSettings.fromRunProfile(env.project, env.runProfile)

		try {
			runSettings.envs = executionGuard.originEnv

			if (SystemInfo.isMac) {
				executionGuard.originPackageManagerPackageRef?.let {
					runSettings.packageManagerPackageRef = it
				}
			}
		} catch (e: Exception) {
			MirrordLogger.logger.error("mirrord failed to clear npm run patch: $e")
			MirrordNotifier.errorNotification("mirrord failed to clear npm run patch",env.project)
		} finally {
			executions.remove(env.executionId)
		}
	}

	override fun processStarting(executorId: String, env: ExecutionEnvironment) {
		if (!MirrordExecManager.enabled || !this.detectNpmRunConfiguration(env)) {
			return super.processStarting(executorId, env)
		}

		executions[env.executionId] = RunConfigGuard(env.executionId)

		val wsl = when (val request = createEnvironmentRequest(env.runProfile, env.project)) {
			is WslTargetEnvironmentRequest -> request.configuration.distribution!!
			else -> null
		}

		patchNpmEnv(wsl, env)

		super.processStartScheduled(executorId, env)
	}

	override fun processStarted(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
		if (this.detectNpmRunConfiguration(env) && executions.containsKey(env.executionId)) {
			clearNpmEnv(env)
		}

		super.processStarted(executorId, env, handler)
	}


	override fun processNotStarted(executorId: String, env: ExecutionEnvironment) {
		if (this.detectNpmRunConfiguration(env) && executions.containsKey(env.executionId)) {
			clearNpmEnv(env)
		}

		super.processNotStarted(executorId, env)
	}


	override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
		if (this.detectNpmRunConfiguration(env) && executions.containsKey(env.executionId)) {
			clearNpmEnv(env)
		}

		return super.processTerminating(executorId, env, handler)
	}
}
