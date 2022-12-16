package com.metalbear.mirrord

import com.intellij.ide.impl.ProjectUtil
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.guessProjectDir
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.util.io.createFile
import com.intellij.util.io.exists
import com.intellij.util.io.write
import java.nio.file.Path


/**
 * Object for interacting with the mirrord config file.
 */
object MirrordConfigAPI {

    const val defaultConfig = """// See documentation here https://mirrord.dev/docs/overview/configuration
{
    "accept_invalid_certificates": false,
    "feature": {
        "network": {
            "incoming": "mirror",
            "outgoing": true
        },
        "fs": "read",
        "env": true
    }
}
    """

    fun getConfigPath(project: Project): Path {
        val basePath = project.basePath ?: throw Error("couldn't resolve project path");
        return Path.of(basePath, ".mirrord", "mirrord.json")
    }

    /**
     * Opens the config file in the editor, creating it if didn't exist before
     */
    fun openConfig(project: Project) {
        val configPath = getConfigPath(project);
        if (!configPath.exists()) {
            configPath.write(defaultConfig, createParentDirs = true)
        }
        val file = VirtualFileManager.getInstance().refreshAndFindFileByNioPath(configPath)!!
        FileEditorManager.getInstance(project).openFile(file, true)
    }
}