package com.metalbear.mirrord

import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.util.xmlb.XmlSerializerUtil
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage


@State(name = "MirrordSettingsState", storages = [Storage("mirrord.xml")])
object MirrordSettingsState : PersistentStateComponent<MirrordSettingsState> {
    var telemetryEnabled: Boolean? = null
    var versionCheckEnabled: Boolean = false

    override fun getState(): MirrordSettingsState {
        return this
    }

    override fun loadState(state: MirrordSettingsState) {
        XmlSerializerUtil.copyBean(state, this)
    }

}