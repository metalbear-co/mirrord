package com.metalbear.mirrord

/**
 * Functions to be called when one of our entry points to the program
 * is called - when process is launched, when go entrypoint, etc
 * It will check to see if it already occured for current run
 * and if it did, it will do nothing
 */
object MirrordExecManager {
    var enabled: Boolean = false

    fun start() {
        if (!enabled) {
            return
        }
        if (!)
    }
}