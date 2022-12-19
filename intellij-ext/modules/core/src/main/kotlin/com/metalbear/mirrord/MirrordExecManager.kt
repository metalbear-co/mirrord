package com.metalbear.mirrord

import com.intellij.execution.wsl.WSLDistribution
import com.intellij.openapi.project.Project

/**
 * Functions to be called when one of our entry points to the program
 * is called - when process is launched, when go entrypoint, etc
 * It will check to see if it already occured for current run
 * and if it did, it will do nothing
 */
object MirrordExecManager {
    private var enabled: Boolean = false

    private fun chooseTarget(project: Project, wslDistribution: WSLDistribution?): String? {
        val pods = MirrordApi.listPods(MirrordConfigAPI.getNamespace(project), project, wslDistribution)
        //                    val mirrordConfigDialog = MirrordDialogBuilder.createDialogBuilder(
//                        MirrordDialogBuilder.createMirrordConfigDialog(
//                            pods,
//                            fileOps,
//                            stealTraffic,
//                            telemetry,
//                            ephemeralContainer,
//                            remoteDns,
//                            tcpOutgoingTraffic,
//                            udpOutgoingTraffic,
//                            agentRustLog,
//                            rustLog,
//                            excludeEnv,
//                            includeEnv,
//                        )
//                    )
//                    if (mirrordConfigDialog.show() == DialogWrapper.OK_EXIT_CODE && !pods.isSelectionEmpty) {
//                        mirrordEnv["MIRRORD_IMPERSONATED_TARGET"] = "pod/${pods.selectedValue}"
////                        mirrordEnv["MIRRORD_TARGET_NAMESPACE"] = podNamespace
//                        mirrordEnv["MIRRORD_FILE_OPS"] = fileOps.isSelected.toString()
//                        mirrordEnv["MIRRORD_AGENT_TCP_STEAL_TRAFFIC"] = stealTraffic.isSelected.toString()
//                        mirrordEnv["MIRRORD_EPHEMERAL_CONTAINER"] = ephemeralContainer.isSelected.toString()
//                        mirrordEnv["MIRRORD_REMOTE_DNS"] = remoteDns.isSelected.toString()
//                        mirrordEnv["MIRRORD_TCP_OUTGOING"] = tcpOutgoingTraffic.isSelected.toString()
//                        mirrordEnv["MIRRORD_UDP_OUTGOING"] = udpOutgoingTraffic.isSelected.toString()
//                        mirrordEnv["MIRRORD_PROGRESS_MODE"] = "off"
//                        mirrordEnv["RUST_LOG"] = (rustLog.selectedItem as LogLevel).name
//                        mirrordEnv["MIRRORD_AGENT_RUST_LOG"] = (agentRustLog.selectedItem as LogLevel).name
//                        if (excludeEnv.text.isNotEmpty()) {
//                            mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE"] = excludeEnv.text.toString()
//                        }
//                        if (includeEnv.text.isNotEmpty()) {
//                            mirrordEnv["MIRRORD_OVERRIDE_ENV_VARS_INCLUDE"] = includeEnv.text.toString()
//                        }
//                        if (!telemetry.isSelected) {
//                            MirrordEnabler.versionCheckDisabled = true
//                        }
//                        val envMap = getRunConfigEnv(env)
//                        envMap?.putAll(mirrordEnv)
//                        envSet = envMap != null
//                } else {
//                        id = ""
//                        defaultFlow = true
//                    }
    }

    /**
     * Starts mirrord, shows dialog for selecting pod if target not set
     * and returns env to set.
     */
    fun start(wslDistribution: WSLDistribution?, project: Project): Map<String, String>? {
        if (!enabled) {
            return null
        }
        if (!MirrordConfigAPI.isTargetSet(project)) {

        }
        MirrordApi.exec()
        return null
    }
}