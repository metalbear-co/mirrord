package com.metalbear.mirrord

import com.intellij.execution.configuration.EnvironmentVariablesData
import com.intellij.execution.configurations.RunProfile
import com.intellij.openapi.project.Project

class MirrordNpmMutableRunSettings(project: Project, private val runSettings: Any) {
    private val myEnvData = runSettings.javaClass.getDeclaredField("myEnvData")
    private val myPackageManagerPackageRef = runSettings.javaClass.getDeclaredField("myPackageManagerPackageRef")

    private val NpmManager = Class.forName(runSettings.javaClass.module, "com.intellij.javascript.nodejs.npm.NpmManager")
    private val NpmNodePackage = Class.forName(runSettings.javaClass.module, "com.intellij.javascript.nodejs.npm.NpmNodePackage")
    private val NodePackageRef = Class.forName(runSettings.javaClass.module, "com.intellij.javascript.nodejs.util.NodePackageRef")

    private val npmMananger = NpmManager.getConstructor(Project::class.java).newInstance(project)

    init {
        myEnvData.isAccessible = true
        myPackageManagerPackageRef.isAccessible = true
    }

    companion object {
        fun fromRunProfile(project: Project, runProfile: RunProfile): MirrordNpmMutableRunSettings {
            val getRunSettings = runProfile.javaClass.getMethod("getRunSettings")
            val runSettings = getRunSettings.invoke(runProfile)

            return MirrordNpmMutableRunSettings(project, runSettings)
        }
    }

    private val packageManagerPackage: Any?
        get() {
            val getPackageRef = NpmManager.getMethod("getPackageRef")
            val setPackageRef = NpmManager.getMethod("setPackageRef", NodePackageRef)

            var originalRef: Any? = null

            if (packageManagerPackageRefIdentifier != "Project") {
                originalRef = getPackageRef.invoke(npmMananger)
                setPackageRef.invoke(npmMananger, packageManagerPackageRef)
            }

            val getPackage = NpmManager.getMethod("getPackage")
            val packageManager = getPackage.invoke(npmMananger)

            originalRef?.let {
                setPackageRef.invoke(npmMananger, it)
            }

            return packageManager
        }

    private val packageManagerPackageRefIdentifier: String
        get() = NodePackageRef.getMethod("getIdentifier").invoke(packageManagerPackageRef) as String

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

    var packageManagerPackageRef: Any
        get() = myPackageManagerPackageRef.get(runSettings)
        set(value) = myPackageManagerPackageRef.set(runSettings, value)

    var packageManagerPackagePath: String?
        get() {
            val getSystemIndependentPath = NpmNodePackage.getMethod("getSystemIndependentPath")
            return packageManagerPackage?.let {
                getSystemIndependentPath.invoke(it) as String
            }
        }
        set(value) {
            value?.let {
                val patchedPackageManager = NpmNodePackage.getConstructor(Class.forName("java.lang.String"))?.newInstance(it)
                val createPackageManagerPackageRef = NodePackageRef.methods.find { m -> m.name == "create" && m.parameterTypes[0].name != "java.lang.String" }

                if (createPackageManagerPackageRef != null && patchedPackageManager != null) {
                    packageManagerPackageRef = createPackageManagerPackageRef.invoke(null, patchedPackageManager)
                }
            }
        }
}