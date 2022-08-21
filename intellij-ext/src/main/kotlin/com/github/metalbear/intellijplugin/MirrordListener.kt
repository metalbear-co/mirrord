package com.github.metalbear.intellijplugin

import com.intellij.execution.ExecutionListener
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBList
import javax.swing.JCheckBox


class MirrordListener : ExecutionListener {
    private val mirrordEnv: LinkedHashMap<String, String> = LinkedHashMap()
    private val log: Logger = Logger.getInstance("MirrordListener")

    init {
        mirrordEnv["DYLD_INSERT_LIBRARIES"] = "target/debug/libmirrord_layer.dylib"
        mirrordEnv["LD_PRELOAD"] = "target/debug/libmirrord_layer.so"
        mirrordEnv["RUST_LOG"] = "DEBUG"
        mirrordEnv["MIRRORD_AGENT_IMPERSONATED_POD_NAME"] = "nginx-deployment-66b6c48dd5-ggd9n"
        mirrordEnv["MIRRORD_ACCEPT_INVALID_CERTIFICATES"] = "true"

        log.debug("Default mirrord environment variables set.")
    }

    companion object {
        var enabled: Boolean = false
        var envSet: Boolean = false
    }

    override fun processStarting(executorId: String, env: ExecutionEnvironment) {
        if (enabled) {
            val kubeDataProvider = KubeDataProvider()

            // Prompt the user to choose a namespace
            val namespaces = JBList(kubeDataProvider.getNamespaces())
            var customDialogBuilder = MirrordDialogBuilder()
            val panel = customDialogBuilder.createMirrordNamespaceDialog(namespaces)
            var dialogBuilder = customDialogBuilder.getDialogBuilder(panel)

            // SUCCESS: Ask the user for the impersonated pod in the chosen namespace
            if (dialogBuilder.show() == DialogWrapper.OK_EXIT_CODE) {
                val choseNamespace = namespaces.selectedValue
                val pods = JBList(kubeDataProvider.getNameSpacedPods(choseNamespace))
                val fileOpsCheckbox = JCheckBox("Enable File Operations")
                val remoteDnsCheckbox = JCheckBox("Enable Remote DNS")
                val panel = customDialogBuilder.createMirrordKubeDialog(pods, fileOpsCheckbox, remoteDnsCheckbox)

                var dialogBuilder = customDialogBuilder.getDialogBuilder(panel)

                // SUCCESS: set the respective environment variables
                if (dialogBuilder.show() == DialogWrapper.OK_EXIT_CODE) {
                    mirrordEnv["MIRRORD_AGENT_IMPERSONATED_POD_NAME"] = pods.selectedValue as String
                    mirrordEnv["MIRRORD_FILE_OPS"] = fileOpsCheckbox.isSelected.toString()
                    mirrordEnv["MIRRORD_REMOTE_DNS"] = remoteDnsCheckbox.isSelected.toString()

                    var envMap = getPythonEnv(env)
                    envMap.putAll(mirrordEnv)

                    log.debug("mirrord env set")
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
            var envMap = getPythonEnv(env)
            for (key in mirrordEnv.keys) {
                if (mirrordEnv.containsKey(key)) {
                    envMap.remove(key)
                }
            }
            log.debug("mirrord env reset")
        }
        super.processTerminating(executorId, env, handler)
    }

    private fun getPythonEnv(env: ExecutionEnvironment): LinkedHashMap<String, String> {
        log.debug("fetching python env")
        var envMethod = env.runProfile.javaClass.getMethod("getEnvs")
        return envMethod.invoke(env.runProfile) as LinkedHashMap<String, String>
    }
}