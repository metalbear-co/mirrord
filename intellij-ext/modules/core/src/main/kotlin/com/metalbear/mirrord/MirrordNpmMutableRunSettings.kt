package com.metalbear.mirrord

import com.intellij.execution.configuration.EnvironmentVariablesData
import com.intellij.openapi.project.Project

class MirrordNpmMutableRunSettings(private val project: Project, private val runSettings: Any) {
    private val myEnvData = runSettings.javaClass.getDeclaredField("myEnvData")
    private val myInterpreterRef = runSettings.javaClass.getDeclaredField("myInterpreterRef")
    private val myPackageManagerPackageRef = runSettings.javaClass.getDeclaredField("myPackageManagerPackageRef")

    init {
        myEnvData.isAccessible = true
        myInterpreterRef.isAccessible = true
        myPackageManagerPackageRef.isAccessible = true
    }

    var envs: Map<String, String>
        get() {
            val envData = myEnvData.get(runSettings) as EnvironmentVariablesData
            return envData.envs
        }
        set(value) {
            val envData = myEnvData.get(runSettings) as EnvironmentVariablesData
            val newEnvData = envData.with(value)

            myEnvData.set(runSettings, newEnvData)
        }

    var interpreterRef: Any
        get() = myInterpreterRef.get(runSettings)
        set(value) = myInterpreterRef.set(runSettings, value)

    var packageManagerPackageRef: Any
        get() = myPackageManagerPackageRef.get(runSettings)
        set(value) = myPackageManagerPackageRef.set(runSettings, value)

    val interpreter: Any
        get() {
            val interpreterRef = interpreterRef
            val resolveInterpreter = interpreterRef.javaClass.methods.find { k -> k.name == "resolve" }
            return resolveInterpreter!!.invoke(interpreterRef, project)
        }

    val packageManager: Any?
        get() {
            val packageManagerPackageRef = packageManagerPackageRef
            val getConstantPackage = packageManagerPackageRef.javaClass.getMethod("getConstantPackage")
            return getConstantPackage.invoke(packageManagerPackageRef)
        }
}