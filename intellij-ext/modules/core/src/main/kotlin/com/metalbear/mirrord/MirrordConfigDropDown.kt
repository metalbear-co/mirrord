@file:Suppress("DialogTitleCapitalization")

package com.metalbear.mirrord

import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.IndexNotReadyException
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.*
import com.intellij.openapi.vfs.newvfs.events.*
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.util.indexing.*
import com.intellij.util.io.EnumeratorStringDescriptor
import com.intellij.util.io.KeyDescriptor
import java.nio.file.Path
import java.util.*
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.JComponent


// [`MirrordConfigIndex`] index is queried once in the update function to initialize the configFiles
// Once initialized, we listen on file events to update the configFiles by querying the index again
// If no config files are present, the chosenFile is set to null, the action is hidden,
// and on pressing the gear icon, a file will be created
// If one file is present, the chosenFile is set to that file, and the action is hidden, it is set as default
// If two or more files are present, the chosenFile is set to the first file, and the action is shown
// Startup -> MirrordConfigIndex -> (MirrordConfigWatcher -> Event -> Query Index -> Update Configs) -> MirrordConfigDropDown
//                                    \<--------------------------------------------------------/
class MirrordConfigDropDown : ComboBoxAction() {

    private lateinit var configFiles: HashSet<String>
    private var blockQueries = AtomicBoolean(false)

    override fun getActionUpdateThread() = ActionUpdateThread.EDT

    // this function is called on click of the dropdown, here we map configFiles -> AnAction
    override fun createPopupActionGroup(button: JComponent, dataContext: DataContext): DefaultActionGroup {
        val project = dataContext.getData(CommonDataKeys.PROJECT) ?: throw Error("couldn't resolve project")
        val actions = configFiles.map { configPath ->
            object : AnAction(getReadablePath(configPath, project)) {
                override fun actionPerformed(e: AnActionEvent) {
                    chosenFile = configPath
                }
            }
        }
        return DefaultActionGroup().apply {
            addAll(actions)
        }
    }

    // mostly what is viewed by the user is done here, goal is to have minimal logic in the function
    // because it is called every half a second
    override fun update(e: AnActionEvent) {
        e.project?.let { project ->
            // this check ensures that we don't query the index when it is being built
            // querying the index during the startup can give us 0
            if (!::configFiles.isInitialized) {
                if (!blockQueries.getAndSet(true)) {
                    object : Task.Backgroundable(project, "mirrord") {
                        override fun run(indicator: ProgressIndicator) {
                            indicator.text = "searching for mirrord config files"
                            initializeConfigFiles(project)
                            blockQueries.set(false)
                        }
                    }.queue()
                }
                return
            }

            if (updateConfigs.getAndSet(false)) {
                if (!blockQueries.getAndSet(true)) {
                    object : Task.Backgroundable(project, "mirrord") {
                        override fun run(indicator: ProgressIndicator) {
                            indicator.text = "updating mirrord config files"
                            updateConfigFiles(project)
                            blockQueries.set(false)
                        }
                    }.queue()
                }
                return
            }

            val files = configFiles
            if (files.size < 2) {
                chosenFile = files.firstOrNull()
                e.presentation.isVisible = false
                return
            }

            chosenFile = chosenFile ?: files.first()

            if (chosenFile !in files) {
                chosenFile = files.first()
            }

            e.presentation.text = getReadablePath(chosenFile!!, project)
            e.presentation.isVisible = true
        }
    }

    private fun initializeConfigFiles(project: Project) {
        val basePath = project.basePath ?: throw Error("couldn't resolve project path")
        val dumbService = DumbService.getInstance(project)
        while (true) {
            dumbService.waitForSmartMode()
            val success = ReadAction.compute<Boolean, RuntimeException> {
                if (project.isDisposed) {
                    throw ProcessCanceledException()
                }
                if (dumbService.isDumb) {
                    return@compute false
                }
                try {
                    configFiles = FileBasedIndex.getInstance().getAllKeys(MirrordConfigIndex.key, project).filter {
                        it.startsWith(basePath)
                    }.toHashSet()
                } catch (e: IndexNotReadyException) {
                    return@compute false
                }
                true
            }
            if (success) {
                break
            }
        }
    }

    private fun updateConfigFiles(project: Project) {
        val updatedConfigFiles = HashSet<String>()
        val basePath = project.basePath ?: throw Error("couldn't resolve project path")
        val dumbService = DumbService.getInstance(project)
        while (true) {
            dumbService.waitForSmartMode()
            val success = ReadAction.compute<Boolean, RuntimeException> {
                if (project.isDisposed) {
                    throw ProcessCanceledException()
                }
                if (dumbService.isDumb) {
                    return@compute false
                }
                try {
                    val allKeys = FileBasedIndex.getInstance().getAllKeys(MirrordConfigIndex.key, project)
                    // to get the updated keys, we need to use this particular method. no other methods such getAllKeys, processAllKeys
                    // give updated values
                    FileBasedIndex.getInstance().processFilesContainingAnyKey(
                        MirrordConfigIndex.key,
                        allKeys,
                        GlobalSearchScope.projectScope(project),
                        null,
                        null
                    ) {
                        if (it.path.startsWith(basePath)) {
                            updatedConfigFiles.add(it.path)
                        }
                        true
                    }
                    configFiles = updatedConfigFiles
                } catch (e: IndexNotReadyException) {
                    return@compute false
                }
                true
            }
            if (success) {
                break
            }
        }
    }

    private fun getReadablePath(path: String, project: Project): String {
        val basePath = project.basePath ?: throw Error("couldn't resolve project path")
        val relativePath = Path.of(basePath).relativize(Path.of(path))
        return relativePath.toString()
    }

    companion object {
        var chosenFile: String? = null
        var updateConfigs: AtomicBoolean = AtomicBoolean(false)
    }
}

// `configFiles` are updated per VFS changes, this approach helps us avoid querying the index on click
// and also updates the configFiles in case the selected file is deleted
class MirrordConfigWatcher : AsyncFileListener {
    override fun prepareChange(events: MutableList<out VFileEvent>): AsyncFileListener.ChangeApplier {
        return object : AsyncFileListener.ChangeApplier {
            override fun afterVfsChange() {
                events.forEach { event ->
                    when (event) {
                        is VFileCreateEvent, is VFileDeleteEvent, is VFileMoveEvent, is VFileCopyEvent -> {
                            MirrordConfigDropDown.updateConfigs.set(true)
                        }
                    }
                }
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
            it.isInLocalFileSystem && !it.isDirectory && it.path.endsWith("mirrord.json")
        }
    }

    override fun dependsOnFileContent(): Boolean {
        return false
    }

}