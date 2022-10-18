package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBList
import javax.swing.JCheckBox
import javax.swing.JTextField


@Suppress("UNCHECKED_CAST", "NAME_SHADOWING")
class MirrordListener : ExecutionListener {
    init {
        val (ldPreloadPath, dylibPath, defaultMirrordAgentLog, rustLog, invalidCertificates, ephemeralContainers) = MirrordDefaultData()

        mirrordEnv["DYLD_INSERT_LIBRARIES"] = dylibPath
        mirrordEnv["LD_PRELOAD"] = ldPreloadPath
        mirrordEnv["MIRRORD_AGENT_RUST_LOG"] = defaultMirrordAgentLog
        mirrordEnv["RUST_LOG"] = rustLog
        mirrordEnv["MIRRORD_ACCEPT_INVALID_CERTIFICATES"] = invalidCertificates.toString()
        mirrordEnv["MIRRORD_EPHEMERAL_CONTAINER"] = ephemeralContainers.toString()
        mirrordEnv["MIRRORD_SKIP_PROCESSES"] = ""
        mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_INCLUDE"] = "*"

    }

    companion object {
        var enabled: Boolean = false
        var envSet: Boolean = false
        var mirrordEnv: LinkedHashMap<String, String> = LinkedHashMap()
    }

    override fun processStartScheduled(executorId: String, env: ExecutionEnvironment) {
        if (enabled) {
            ApplicationManager.getApplication().invokeLater {
                val customDialogBuilder = MirrordDialogBuilder()
                val kubeDataProvider = KubeDataProvider()

                val namespaces = try {
                    JBList(kubeDataProvider.getNamespaces())
                } catch (e: Exception) {
                    MirrordEnabler.notify(
                        "Error occurred while fetching namespaces from Kubernetes context",
                        NotificationType.ERROR,
                        env.project
                    )
                    return@invokeLater super.processStartScheduled(executorId, env)
                }
                val panel = customDialogBuilder.createMirrordNamespaceDialog(namespaces)
                val dialogBuilder = customDialogBuilder.getDialogBuilder(panel)

                // SUCCESS: Ask the user for the impersonated pod in the chosen namespace
                if (dialogBuilder.show() == DialogWrapper.OK_EXIT_CODE && !namespaces.isSelectionEmpty) {
                    val choseNamespace = namespaces.selectedValue

                    val pods = try {
                        JBList(kubeDataProvider.getNameSpacedPods(choseNamespace))
                    } catch (e: Exception) {
                        MirrordEnabler.notify(
                            "Error occurred while fetching pods from Kubernetes context",
                            NotificationType.ERROR,
                            env.project
                        )
                        return@invokeLater super.processStartScheduled(executorId, env)
                    }

                    val fileOps = JCheckBox("File Operations")
                    fileOps.isSelected = true
                    val remoteDns = JCheckBox("Remote DNS")
                    remoteDns.isSelected = true
                    val outgoingTraffic = JCheckBox("Outgoing Traffic")
                    outgoingTraffic.isSelected = true
                    val trafficStealing = JCheckBox("Traffic Stealing")

                    val ephemeralContainerCheckBox = JCheckBox("Enable Ephemeral Containers")

                    val agentRustLog = JTextField(mirrordEnv["MIRRORD_AGENT_RUST_LOG"])
                    val rustLog = JTextField(mirrordEnv["RUST_LOG"])
                    val excludeEnv = JTextField("")
                    val includeEnv = JTextField(mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_INCLUDE"])

                    val panel = customDialogBuilder.createMirrordKubeDialog(
                        pods,
                        fileOps,
                        remoteDns,
                        outgoingTraffic,
                        trafficStealing,
                        ephemeralContainerCheckBox,
                        agentRustLog,
                        rustLog,
                        excludeEnv,
                        includeEnv,
                    )
                    val dialogBuilder = customDialogBuilder.getDialogBuilder(panel)

                    // SUCCESS: set the respective environment variables
                    if (dialogBuilder.show() == DialogWrapper.OK_EXIT_CODE && !pods.isSelectionEmpty) {
                        mirrordEnv["MIRRORD_AGENT_IMPERSONATED_POD_NAME"] = pods.selectedValue as String
                        mirrordEnv["MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE"] = choseNamespace as String
                        mirrordEnv["MIRRORD_FILE_OPS"] = fileOps.isSelected.toString()
                        mirrordEnv["MIRRORD_AGENT_TCP_STEAL_TRAFFIC"] = trafficStealing.isSelected.toString()
                        mirrordEnv["MIRRORD_EPHEMERAL_CONTAINER"] = ephemeralContainerCheckBox.isSelected.toString()
                        mirrordEnv["MIRRORD_REMOTE_DNS"] = remoteDns.isSelected.toString()
                        mirrordEnv["MIRRORD_TCP_OUTGOING"] = outgoingTraffic.isSelected.toString()
                        mirrordEnv["MIRRORD_UDP_OUTGOING"] = outgoingTraffic.isSelected.toString()
                        mirrordEnv["RUST_LOG"] = rustLog.text.toString()
                        mirrordEnv["MIRRORD_AGENT_RUST_LOG"] = agentRustLog.text.toString()
                        mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE"] = excludeEnv.text.toString()
                        mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_INCLUDE"] = includeEnv.text.toString()
                        val envMap = getRunConfigEnv(env)
                        envMap?.putAll(mirrordEnv)

                        envSet = envMap != null
                    }
                }
            }
        }
        // FAILURE: Just call the parent implementation
        return super.processStartScheduled(executorId, env)
    }

    override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
        // NOTE: If the option was enabled, and we actually set the env, i.e. cancel was not clicked on the dialog,
        // we clear up the Environment, because we don't want mirrord to run again if the user hits debug again
        // with mirrord toggled off.
        if (enabled and envSet) {
            if (env.runProfile::class.simpleName == "GoApplicationConfiguration") {
                GoRunConfig.clearGoEnv()
                return super.processTerminating(executorId, env, handler)
            }
            val envMap = try {
                val envMethod = env.runProfile.javaClass.getMethod("getEnvs")
                envMethod.invoke(env.runProfile) as LinkedHashMap<String, String>
            } catch (e: Exception) {
                MirrordEnabler.notify(
                    "Error occurred while removing mirrord environment",
                    NotificationType.ERROR,
                    env.project
                )
                return super.processTerminating(executorId, env, handler)
            }
            for (key in mirrordEnv.keys) {
                if (envMap.containsKey(key)) {
                    envMap.remove(key)
                }
            }
        }
        return super.processTerminating(executorId, env, handler)
    }

    private fun getRunConfigEnv(env: ExecutionEnvironment): LinkedHashMap<String, String>? {
        if (env.runProfile::class.simpleName == "GoApplicationConfiguration")
            return null
        return try {
            val envMethod = env.runProfile.javaClass.getMethod("getEnvs")
            envMethod.invoke(env.runProfile) as LinkedHashMap<String, String>
        } catch (e: Exception) {
            MirrordEnabler.notify(
                "Error occurred while substituting provided configuration",
                NotificationType.ERROR,
                env.project
            )
            null
        }
    }
}