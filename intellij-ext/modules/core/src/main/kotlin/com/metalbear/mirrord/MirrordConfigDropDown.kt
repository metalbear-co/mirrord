package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.AsyncFileListener
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.openapi.vfs.newvfs.events.VFileCreateEvent
import com.intellij.openapi.vfs.newvfs.events.VFileDeleteEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.openapi.vfs.newvfs.events.VFileMoveEvent
import com.intellij.util.indexing.*
import com.intellij.util.io.EnumeratorStringDescriptor
import com.intellij.util.io.KeyDescriptor
import java.nio.file.Path
import java.util.*
import javax.swing.JComponent
import kotlin.collections.HashSet


// [`MirrordConfigIndex`] index is queried once in the update function to initialize the configFiles
// Once initialized, we listen on file events to update the configFiles, using [`MirrordConfigWatcher`]
// If no config files are present, the chosenFile is set to null, the action is hidden,
// and on pressing the gear icon, a file will be created
// If one file is present, the chosenFile is set to that file, and the action is hidden, it is set as default
// If two or more files are present, the chosenFile is set to the first file, and the action is shown
class MirrordConfigDropDown : ComboBoxAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        val project = dataContext.getData(CommonDataKeys.PROJECT) ?: throw Error("couldn't resolve project")
        val actions = configFiles?.map { configPath ->
            object : AnAction(getReadablePath(configPath, project)) {
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
        e.project?.let { project ->
            // this check ensures that we don't query the index when it is being built
            // querying the index during the startup can give us 0
            if (DumbService.isDumb(project)) {
                e.presentation.isVisible = false
                return
            }

            initializeConfigFiles(project)

            val files = configFiles ?: return
            if (files.size < 2) {
                chosenFile = files.firstOrNull()
                e.presentation.isVisible = false
                return
            }

            chosenFile = chosenFile ?: files.first()

            if (chosenFile !in files) {
                chosenFile = files.first()
            }

            e.presentation.text = getReadablePath(chosenFile!!, e.project!!)
            e.presentation.isVisible = true
        }
    }


    private fun initializeConfigFiles(project: Project) {
        configFiles ?: run {
            configFiles = FileBasedIndex.getInstance().getAllKeys(MirrordConfigIndex.key, project).toHashSet()
        }

    }

    private fun getReadablePath(path: String, project: Project): String {
        val basePath = project.basePath ?: throw Error("couldn't resolve project path")
        val relativePath = Path.of(basePath).relativize(Path.of(path))
        return relativePath.toString()
    }


    companion object {
        var chosenFile: String? = null
        var configFiles: HashSet<String>? = null
    }
}

// Based on virtual file events, we update our configs since indexes are always not
class MirrordConfigWatcher : AsyncFileListener {
    override fun prepareChange(events: MutableList<out VFileEvent>): AsyncFileListener.ChangeApplier {
        val addPaths = HashSet<String>()
        val removePaths = HashSet<String>()
        // TODO: handle the remaining events/check their relevance
        events.forEach { it ->
            when (it) {
                is VFileCreateEvent -> {
                    // TODO: handle directory based events
                    // check if it is a directory
                    // this does not work at all, no virtual file is created here
                    // for example, if someone creates a directory
                    if (it.file?.isDirectory == true) {
                        it.file!!.children.forEach { child ->
                            if (child.path.endsWith("mirrord.json")) {
                                addPaths.add(child.path)
                            }
                        }
                    } else {
                        it.takeIf { it.path.endsWith("mirrord.json") }?.let {
                            addPaths.add(it.path)
                        }
                    }
                }

                is VFileDeleteEvent -> {
                    // case where it is a directory
                    if (it.file.isDirectory) {
                        it.file.children.forEach { child ->
                            child.takeIf { it.path.endsWith("mirrord.json") }?.let {
                                removePaths.add(it.path)
                            }
                        }
                    } else {
                        it.takeIf { it.path.endsWith("mirrord.json") }?.let {
                            removePaths.add(it.path)
                        }
                    }
                }

                is VFileMoveEvent -> {
                    if (it.path.endsWith("mirrord.json")) {
                        removePaths.add(it.path)
                        addPaths.add(it.newPath)
                    }

                    // case where it is a directory
                    if (it.file.isDirectory) {
                        val children = it.file.children
                        children.forEach { child ->
                            if (child.path.endsWith("mirrord.json")) {
                                removePaths.add(child.path)
                            }
                            // now since the event is a directory, we don't get the new paths for children
                            // we need the VirtualFile for the new path
                            val virtualFile =
                                VirtualFileManager.getInstance().refreshAndFindFileByNioPath(Path.of(it.newPath))
                            virtualFile?.children?.forEach { newChild ->
                                if (newChild.path.endsWith("mirrord.json")) {
                                    addPaths.add(newChild.path)
                                }
                            }
                        }
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

// An index for mirrord config files, mapping filePath -> Void
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