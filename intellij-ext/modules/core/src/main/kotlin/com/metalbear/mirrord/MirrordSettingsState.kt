package com.metalbear.mirrord

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.*


@State(name = "MirrordSettingsState", storages = [Storage("mirrord.xml")])
open class MirrordSettingsState : PersistentStateComponent<MirrordSettingsState.MirrordState> {
    companion object {
        val instance: MirrordSettingsState
            get() = ApplicationManager.getApplication().getService(MirrordSettingsState::class.java)
    }

    var mirrordState: MirrordState = MirrordState()

    override fun getState(): MirrordState {
        return mirrordState
    }

    // after automatically loading our save state,  we will keep reference to it
    override fun loadState(state: MirrordState) {
        mirrordState = state
    }


    class MirrordState {
        var telemetryEnabled: Boolean? = null
        var versionCheckEnabled: Boolean = false
        var lastChosenTarget: String? = null
    }
}