package com.metalbear.mirrord
import io.kubernetes.client.openapi.apis.CoreV1Api
import io.kubernetes.client.util.Config

class KubeDataProvider : CoreV1Api(Config.defaultClient()) {
    fun getNameSpacedPods(namespace: String): List<String> {
        val pods = listNamespacedPod(namespace, null, null, null, null, null, null, null, null, null, null)
        return pods.items.map{ it.metadata!!.name!! }
    }

    fun getNamespaces() : List<String> {
        val namespaces = listNamespace(null, null, null, null, null, null, null, null, null, null)
        return namespaces.items.map{ it.metadata!!.name!! }
    }
}