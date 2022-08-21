package com.github.metalbear.intellijplugin
import io.kubernetes.client.openapi.apis.CoreV1Api
import io.kubernetes.client.util.Config

class KubeDataProvider : CoreV1Api(Config.defaultClient()) {
    fun getNameSpacedPods(namespace: String): ArrayList<String> {
        val pods = listNamespacedPod(namespace, null, null, null, null, null, null, null, null, null, null)

        var list = ArrayList<String>()
        for (pod in pods.items) {
            list.add(pod.metadata!!.name!!);
        }
        return list
    }

    fun getNamespaces() :ArrayList<String> {
        var namespaces = listNamespace(null, null, null, null, null, null, null, null, null, null)
        var list = ArrayList<String>()
        for (namespace in namespaces.items) {
            list.add(namespace.metadata!!.name!!)
        }
        return list
    }
}