package com.metalbear.mirrord.products.webstorm

import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.options.SettingsEditor
import com.intellij.openapi.util.JDOMExternalizerUtil
import com.intellij.openapi.util.Key
import org.jdom.Element
import javax.swing.JComponent
import javax.swing.JPanel

class RunConfigurationSettingsEditor<T : RunConfigurationBase<*>>(configuration: RunConfigurationBase<*>?) :
    SettingsEditor<T>() {
    private val editor: JPanel = JPanel()

    override fun resetEditorFrom(configuration: T) {
//        val state: DirenvSettings = configuration!!.getCopyableUserData(USER_DATA_KEY)
//        if (state != null) {
//            editor.setState(state)
//        }
    }

    @Throws(ConfigurationException::class)
    override fun applyEditorTo(configuration: T) {
//        configuration.putCopyableUserData(USER_DATA_KEY, )
    }

    override fun createEditor(): JComponent {
        return editor
    }

//    companion object {
//        val USER_DATA_KEY: Key<DirenvSettings> = Key<DirenvSettings>("Direnv Settings")
//        private const val FIELD_DIRENV_ENABLED = "DIRENV_ENABLED"
//        private const val FIELD_DIRENV_TRUSTED = "DIRENV_TRUSTED"
//        fun readExternal(configuration: RunConfigurationBase<*>, element: Element) {
//            val isDirenvEnabled = readBool(element, FIELD_DIRENV_ENABLED)
//            val isDirenvTrusted = readBool(element, FIELD_DIRENV_TRUSTED)
//            val state = DirenvSettings(isDirenvEnabled, isDirenvTrusted)
//            configuration.putCopyableUserData(USER_DATA_KEY, state)
//        }
//
//        fun writeExternal(configuration: RunConfigurationBase<*>, element: Element) {
//            val state: DirenvSettings = configuration.getCopyableUserData(USER_DATA_KEY)
//            if (state != null) {
//                writeBool(element, FIELD_DIRENV_ENABLED, state.isDirenvEnabled())
//                writeBool(element, FIELD_DIRENV_TRUSTED, state.isDirenvTrusted())
//            }
//        }
//
//        private fun readBool(element: Element, field: String): Boolean {
//            val isDirenvEnabledStr = JDOMExternalizerUtil.readField(element, field)
//            return java.lang.Boolean.parseBoolean(isDirenvEnabledStr)
//        }
//
//        private fun writeBool(element: Element, field: String, value: Boolean) {
//            JDOMExternalizerUtil.writeField(element, field, java.lang.Boolean.toString(value))
//        }
//
//        fun collectEnv(
//            runConfigurationBase: RunConfigurationBase<*>,
//            workingDirectory: String?,
//            runConfigEnv: Map<String, String>?
//        ): Map<String, String> {
//            val envVars: MutableMap<String, String> = HashMap(runConfigEnv)
//            val state: DirenvSettings = runConfigurationBase.getCopyableUserData(USER_DATA_KEY)
//            envVars.putAll(collectEnv(state, workingDirectory))
//            return envVars
//        }
//
//        fun collectEnv(state: DirenvSettings?, workingDirectory: String?): Map<String, String> {
//            val envVars: MutableMap<String, String> = HashMap()
//            if (state != null && state.isDirenvEnabled()) {
//                val cmd = DirenvCmd(workingDirectory)
//                envVars.putAll(cmd.importDirenv(state.isDirenvTrusted()))
//            }
//            return envVars
//        }
//
//        fun getState(configuration: RunConfigurationBase<*>): DirenvSettings {
//            return configuration.getCopyableUserData(USER_DATA_KEY)
//        }
//
//        val editorTitle: String
//            get() = "Direnv"
//    }
}