package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.AsyncFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileCreateEvent
import com.intellij.openapi.vfs.newvfs.events.VFileDeleteEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.openapi.vfs.newvfs.events.VFileMoveEvent
import com.intellij.util.indexing.*
import com.intellij.util.io.EnumeratorStringDescriptor
import com.intellij.util.io.KeyDescriptor
import java.util.*
import javax.swing.JComponent
import kotlin.collections.HashSet

class MirrordConfigDropDown : ComboBoxAction() {
    private var chosenFile: String? = null

    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        val currentProject = CommonDataKeys.PROJECT.getData(dataContext) ?: throw Error("No project found")
        initializeConfigFiles(currentProject)
        val actions = configFiles?.map { configPath ->
            object : AnAction(configPath) {
                override fun actionPerformed(e: AnActionEvent) {
                    chosenFile = configPath
                }
            }
        }
        return DefaultActionGroup().apply {
            actions?.let { addAll(it) }
        }
    }

    override fun update(e: AnActionEvent) {
        e.presentation.apply {
            text = chosenFile ?: "Select Configuration"
        }
    }

    private fun initializeConfigFiles(project: Project) {
        configFiles ?: run {
            configFiles = FileBasedIndex.getInstance().getAllKeys(MirrordConfigIndex.key, project).toHashSet()
        }

    }

    companion object {
        var configFiles: HashSet<String>? = null
    }
}


class MirrordConfigWatcher : AsyncFileListener {
    override fun prepareChange(events: MutableList<out VFileEvent>): AsyncFileListener.ChangeApplier {
        var addPaths = HashSet<String>()
        var removePaths = HashSet<String>()
        events.forEach {
            when (it) {
                is VFileCreateEvent -> {
                    if (it.path.endsWith("mirrord.json")) {
                        addPaths.add(it.path)
                    }
                }
                is VFileDeleteEvent -> {
                    if (it.path.endsWith("mirrord.json")) {
                        removePaths.add(it.path)
                    }
                    // case where it is a directory
                    if (it.file.isDirectory) {
                        // we need to check  for children
                        val children = it.file.children
                        children.forEach { child ->
                            if (child.path.endsWith("mirrord.json")) {
                                removePaths.add(child.path)
                            }
                        }
                    }
                }

                is VFileMoveEvent -> {
                    if (it.path.endsWith("mirrord.json")) {
                        removePaths.add(it.path)
                        addPaths.add(it.newPath)
                    }
                }
            }
        }
        return object : AsyncFileListener.ChangeApplier {
            override fun afterVfsChange() {
                MirrordConfigDropDown.configFiles?.addAll(addPaths)
                MirrordConfigDropDown.configFiles?.removeAll(removePaths)

            }
        }
    }
}

class MirrordConfigIndex : ScalarIndexExtension<String>() {

    companion object {
        val key = ID.create<String, Void>("mirrordConfig")
    }

    override fun getName(): ID<String, Void> {
        return key
    }

    override fun getIndexer(): DataIndexer<String, Void, FileContent> {
        return DataIndexer {
            val path = it.file.path
            Collections.singletonMap<String, Void>(path, null)
        }
    }

    override fun getKeyDescriptor(): KeyDescriptor<String> {
        return EnumeratorStringDescriptor.INSTANCE
    }

    override fun getVersion(): Int {
        return 0
    }

    override fun getInputFilter(): FileBasedIndex.InputFilter {
        return FileBasedIndex.InputFilter {
            // TODO: replace with a proper regular expression
            it.isInLocalFileSystem && !it.isDirectory && it.path.endsWith("mirrord.json")
        }
    }

    override fun dependsOnFileContent(): Boolean {
        return false
    }

}