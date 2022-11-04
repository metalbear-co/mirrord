package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBList
import javax.swing.JCheckBox
import javax.swing.JComboBox
import javax.swing.JTextField
import javax.swing.ListSelectionModel


class MirrordListener : ExecutionListener {

    private val defaults = MirrordDefaultConfig()

    init {
        mirrordEnv["DYLD_INSERT_LIBRARIES"] = defaults.dylibPath
        mirrordEnv["LD_PRELOAD"] = defaults.ldPreloadPath
        mirrordEnv["MIRRORD_ACCEPT_INVALID_CERTIFICATES"] = defaults.acceptInvalidCertificates.toString()
        mirrordEnv["MIRRORD_SKIP_PROCESSES"] = defaults.skipProcesses
        mirrordEnv["DEBUGGER_IGNORE_PORTS_PATCH"] = defaults.ignorePorts
    }

    companion object {
        var id: String = ""
        var enabled: Boolean = false
            set(value) {
                id = ""
                field = value
            }
        var envSet: Boolean = false
        var mirrordEnv: LinkedHashMap<String, String> = LinkedHashMap()
    }

    override fun processStartScheduled(executorId: String, env: ExecutionEnvironment) {
        if (enabled && id.isEmpty()) {
            id = executorId // id is set here to make sure we don't spawn the dialog twice
            ApplicationManager.getApplication().invokeLater {
                val kubeDataProvider = KubeDataProvider()

                val namespaces = try {
                    kubeDataProvider.getNamespaces().asJBList()
                } catch (e: Exception) {
                    MirrordEnabler.notify(
                        "Error occurred while fetching namespaces from Kubernetes context.\n " +
                                "mirrord will use the default namespace from kube configuration.\n `${e.message}`",
                        NotificationType.ERROR,
                        env.project
                    )
                    null
                }

                // we need the following check to make sure in case the dialog is spawned, we get an
                // OK exit code and a non-empty selection from the user
                val showNamespaceDialog = if (namespaces != null) {
                    val namespaceDialog = namespaces.let { MirrordDialogBuilder.createMirrordNamespaceDialog(it) }.let {
                        MirrordDialogBuilder.createDialogBuilder(
                            it
                        )
                    }
                    namespaceDialog.show() == DialogWrapper.OK_EXIT_CODE && !namespaces.isSelectionEmpty
                } else {
                    true
                }

                val podNamespace = namespaces?.selectedValue ?: kubeDataProvider.getPodNamespace()

                if (showNamespaceDialog) {
                    val pods = try {
                        kubeDataProvider.getNameSpacedPods(podNamespace).asJBList()
                    } catch (e: Exception) {
                        MirrordEnabler.notify(
                            "Error occurred while fetching pods from Kubernetes context: `${e.message}`",
                            NotificationType.ERROR,
                            env.project
                        )
                        return@invokeLater super.processStartScheduled(executorId, env)
                    }

                    val fileOps = JCheckBox("File Operations", defaults.fileOps)
                    val stealTraffic = JCheckBox("TCP Steal Traffic", defaults.stealTraffic)
                    val telemetry = JCheckBox("Telemetry", defaults.telemetry)
                    val ephemeralContainer = JCheckBox("Use Ephemeral Container", defaults.ephemeralContainers)
                    val remoteDns = JCheckBox("Remote DNS", defaults.remoteDns)
                    val tcpOutgoingTraffic = JCheckBox("TCP Outgoing Traffic", defaults.tcpOutgoingTraffic)
                    val udpOutgoingTraffic = JCheckBox("UDP Outgoing Traffic", defaults.udpOutgoingTraffic)


                    val agentRustLog = JComboBox(LogLevel.values())
                    agentRustLog.selectedItem = defaults.agentRustLog
                    val rustLog = JComboBox(LogLevel.values())
                    rustLog.selectedItem = defaults.rustLog

                    val excludeEnv = JTextField(defaults.overrideEnvVarsExclude)
                    val includeEnv = JTextField(defaults.overrideEnvVarsInclude)

                    val mirrordConfigDialog = MirrordDialogBuilder.createDialogBuilder(
                        MirrordDialogBuilder.createMirrordConfigDialog(
                            pods,
                            fileOps,
                            stealTraffic,
                            telemetry,
                            ephemeralContainer,
                            remoteDns,
                            tcpOutgoingTraffic,
                            udpOutgoingTraffic,
                            agentRustLog,
                            rustLog,
                            excludeEnv,
                            includeEnv,
                        )
                    )
                    if (mirrordConfigDialog.show() == DialogWrapper.OK_EXIT_CODE && !pods.isSelectionEmpty) {
                        mirrordEnv["MIRRORD_IMPERSONATED_TARGET"] = "pod/${pods.selectedValue}"
                        mirrordEnv["MIRRORD_TARGET_NAMESPACE"] = podNamespace
                        mirrordEnv["MIRRORD_FILE_OPS"] = fileOps.isSelected.toString()
                        mirrordEnv["MIRRORD_AGENT_TCP_STEAL_TRAFFIC"] = stealTraffic.isSelected.toString()
                        mirrordEnv["MIRRORD_EPHEMERAL_CONTAINER"] = ephemeralContainer.isSelected.toString()
                        mirrordEnv["MIRRORD_REMOTE_DNS"] = remoteDns.isSelected.toString()
                        mirrordEnv["MIRRORD_TCP_OUTGOING"] = tcpOutgoingTraffic.isSelected.toString()
                        mirrordEnv["MIRRORD_UDP_OUTGOING"] = udpOutgoingTraffic.isSelected.toString()
                        mirrordEnv["RUST_LOG"] = (rustLog.selectedItem as LogLevel).name
                        mirrordEnv["MIRRORD_AGENT_RUST_LOG"] = (agentRustLog.selectedItem as LogLevel).name
                        if (excludeEnv.text.isNotEmpty()) {
                            mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE"] = excludeEnv.text.toString()
                        }
                        if (includeEnv.text.isNotEmpty()) {
                            mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_INCLUDE"] = includeEnv.text.toString()
                        }
                        if (!telemetry.isSelected) {
                            MirrordEnabler.versionCheckDisabled = true
                        }
                        val envMap = getRunConfigEnv(env)
                        envMap?.putAll(mirrordEnv)
                        envSet = envMap != null
                    }
                } else {
                    id = ""
                }
            }
        }
        // FAILURE: Just call the parent implementation
        return super.processStartScheduled(executorId, env)
    }

    private fun List<String>.asJBList() = JBList(this).apply {
        selectionMode = ListSelectionModel.SINGLE_SELECTION
    }

    override fun processNotStarted(executorId: String, env: ExecutionEnvironment) {
        id = ""
        envSet = false
        super.processNotStarted(executorId, env)
    }

    @Suppress("UNCHECKED_CAST")
    override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
        // NOTE: If the option was enabled, and we actually set the env, i.e. cancel was not clicked on the dialog,
        // we clear up the Environment, because we don't want mirrord to run again if the user hits debug again
        // with mirrord toggled off.
        if (enabled && envSet && executorId == id) {
            id = ""
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

    @Suppress("UNCHECKED_CAST")
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