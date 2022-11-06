package com.metalbear.mirrord

import io.kubernetes.client.openapi.apis.CoreV1Api
import io.kubernetes.client.util.ClientBuilder
import io.kubernetes.client.util.Config
import io.kubernetes.client.util.KubeConfig
import io.kubernetes.client.util.Namespaces
import java.io.File

class KubeDataProvider : CoreV1Api(Config.defaultClient()) {
    fun readNamespaceFromKubeConfig(): String {
        var method = ClientBuilder::class.java.getDeclaredMethod("findConfigFromEnv")
        method.isAccessible = true
        var kubeConfig = method.invoke(null) as File
        var loadedKubeConfig = KubeConfig.loadKubeConfig(kubeConfig.reader())
        if (loadedKubeConfig != null) {
            return loadedKubeConfig.namespace ?: Namespaces.NAMESPACE_DEFAULT
        }
        method = ClientBuilder::class.java.getDeclaredMethod("findConfigInHomeDir")
        method.isAccessible = true
        kubeConfig = method.invoke(null) as File
        loadedKubeConfig = KubeConfig.loadKubeConfig(kubeConfig.reader())
        if (loadedKubeConfig != null) {
            return loadedKubeConfig.namespace ?: Namespaces.NAMESPACE_DEFAULT
        }
        return Namespaces.NAMESPACE_DEFAULT
    }

    fun getNameSpacedPods(namespace: String): List<String> {
        val pods = listNamespacedPod(namespace, null, null, null, null, null, null, null, null, null, null)
        return pods.items.map { it.metadata!!.name!! }
    }

    fun getNamespaces(): List<String> {
        val namespaces = listNamespace(null, null, null, null, null, null, null, null, null, null)
        return namespaces.items.map { it.metadata!!.name!! }
    }
}