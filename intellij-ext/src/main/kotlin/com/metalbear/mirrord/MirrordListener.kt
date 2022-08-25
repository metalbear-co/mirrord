package com.metalbear.mirrord

import com.intellij.execution.ExecutionListener
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBList
import javax.swing.JCheckBox
import javax.swing.JTextField


class MirrordListener : ExecutionListener {
    private val mirrordEnv: LinkedHashMap<String, String> = LinkedHashMap()

    init {
        val (ldPreloadPath, dylibPath, defaultMirrordAgentLog, rustLog, invalidCertificates, ephemeralContainers) = MirrordDefaultData()

        mirrordEnv["DYLD_INSERT_LIBRARIES"] = dylibPath
        mirrordEnv["LD_PRELOAD"] = ldPreloadPath
        mirrordEnv["MIRRORD_AGENT_RUST_LOG"] = defaultMirrordAgentLog
        mirrordEnv["RUST_LOG"] = rustLog
        mirrordEnv["MIRRORD_ACCEPT_INVALID_CERTIFICATES"] = invalidCertificates.toString()
        mirrordEnv["MIRRORD_EPHEMERAL_CONTAINER"] = ephemeralContainers.toString()

    }

    companion object {
        var enabled: Boolean = false
        var envSet: Boolean = false
    }

    override fun processStarting(executorId: String, env: ExecutionEnvironment) {

        if (enabled) {
            val customDialogBuilder = MirrordDialogBuilder()
            val kubeDataProvider = KubeDataProvider()

            // Prompt the user to choose a namespace
            val namespaces = JBList(kubeDataProvider.getNamespaces())
            val panel = customDialogBuilder.createMirrordNamespaceDialog(namespaces)
            val dialogBuilder = customDialogBuilder.getDialogBuilder(panel)

            // SUCCESS: Ask the user for the impersonated pod in the chosen namespace
            if (dialogBuilder.show() == DialogWrapper.OK_EXIT_CODE) {
                val choseNamespace = namespaces.selectedValue
                val pods = JBList(kubeDataProvider.getNameSpacedPods(choseNamespace))

                val fileOpsCheckbox = JCheckBox("Enable File Operations")
                val remoteDnsCheckbox = JCheckBox("Enable Remote DNS")
                val ephemeralContainerCheckBox = JCheckBox("Enable Ephemeral Containers")

                val agentRustLog = JTextField(mirrordEnv["MIRRORD_AGENT_RUST_LOG"])
                val rustLog = JTextField(mirrordEnv["RUST_LOG"])

                val panel = customDialogBuilder.createMirrordKubeDialog(pods, fileOpsCheckbox, remoteDnsCheckbox, ephemeralContainerCheckBox, agentRustLog, rustLog)
                val dialogBuilder = customDialogBuilder.getDialogBuilder(panel)

                // SUCCESS: set the respective environment variables
                if (dialogBuilder.show() == DialogWrapper.OK_EXIT_CODE && pods.selectedValue != null) {
                    mirrordEnv["MIRRORD_AGENT_IMPERSONATED_POD_NAME"] = pods.selectedValue as String
                    mirrordEnv["MIRRORD_FILE_OPS"] = fileOpsCheckbox.isSelected.toString()
                    mirrordEnv["MIRRORD_EPHEMERAL_CONTAINER"] = ephemeralContainerCheckBox.isSelected.toString()
                    mirrordEnv["MIRRORD_REMOTE_DNS"] = remoteDnsCheckbox.isSelected.toString()
                    mirrordEnv["MIRRORD_AGENT_RUST_LOG"] = agentRustLog.text.toString()

                    val envMap = getRunConfigEnv(env)
                    envMap.putAll(mirrordEnv)

                    envSet = true
                }
            }
        }
        // FAILURE: Just call the parent implementation
        super.processStarting(executorId, env)
    }

    override fun processTerminating(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler) {
        // NOTE: If the option was enabled, and we actually set the env, i.e. cancel was not clicked on the dialog,
        // we clear up the Environment, because we don't want mirrord to run again if the user hits debug again
        // with mirrord toggled off.
        if (enabled and envSet) {
            val envMap = getRunConfigEnv(env)
            for (key in mirrordEnv.keys) {
                if (mirrordEnv.containsKey(key)) {
                    envMap.remove(key)
                }
            }
        }
        super.processTerminating(executorId, env, handler)
    }

    private fun getRunConfigEnv(env: ExecutionEnvironment): LinkedHashMap<String, String> {
        val envMethod = env.runProfile.javaClass.getMethod("getEnvs")
        return envMethod.invoke(env.runProfile) as LinkedHashMap<String, String>
    }
}