package com.metalbear.mirrord
//
import com.goide.execution.extension.GoExecutorExtension
import com.intellij.openapi.module.Module
import com.intellij.openapi.project.Project

class MirrordGoExecListener : GoExecutorExtension() {
    override fun getExtraEnvironment(project: Project, module: Module?, currentEnvironment: MutableMap<String, String>): MutableMap<String, String>? {
        print("hi")
        return HashMap<String,String>();
    }
}