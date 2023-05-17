@file:Suppress("DialogTitleCapitalization")

package com.metalbear.mirrord

import com.intellij.ide.DataManager
import com.intellij.openapi.actionSystem.*
import com.intellij.openapi.actionSystem.ex.ComboBoxAction
import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.IndexNotReadyException
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.AsyncFileListener
import com.intellij.openapi.vfs.newvfs.events.*
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.util.indexing.*
import com.intellij.util.io.EnumeratorStringDescriptor
import com.intellij.util.io.KeyDescriptor
import java.nio.file.Path
import java.util.*
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

    @Volatile
    private var blockQueries: Boolean = false

    override fun getActionUpdateThread() = ActionUpdateThread.BGT

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

    @Deprecated(
        "Deprecated in Java", ReplaceWith(
            "createPopupActionGroup(button, DataManager.getInstance().getDataContext(button))",
            "com.intellij.ide.DataManager"
        )
    )
    override fun createPopupActionGroup(button: JComponent): DefaultActionGroup {
        return createPopupActionGroup(button, DataManager.getInstance().getDataContext(button))
    }

    private fun getReadablePath(path: String, project: Project): String {
        val basePath = project.basePath ?: throw Error("couldn't resolve project path")
        val relativePath = Path.of(basePath).relativize(Path.of(path))
        return relativePath.toString()
    }

    // mostly what is viewed by the user is done here, goal is to have minimal logic in the function
    // because it is called every half a second
    override fun update(e: AnActionEvent) {
        e.project?.let { project ->
            // this check ensures that we don't query the index when it is being built
            // querying the index during the startup/Dumb Mode can give us 0 or stale data
            if (!::configFiles.isInitialized) {
                e.presentation.isVisible = false
                blockAndQueryIndex(project)
                return
            }

            blockAndQueryIndex(project)

            if (configFiles.size > 1) {
                if (chosenFile !in configFiles) {
                    chosenFile = configFiles.first()
                }

                e.presentation.text = chosenFile?.let { getReadablePath(it, project) }
            } else {
                chosenFile = configFiles.firstOrNull()
            }
            e.presentation.isVisible = configFiles.size > 1
        }
    }

    private fun updateConfigFiles(project: Project) {
        val updatedConfigFiles = HashSet<String>()
        val basePath = project.basePath ?: throw Error("couldn't resolve project path")
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
    }

    // this function spawns a background task to query the index, not blocking the current thread
    // it maintains a single task to do so, using the `blockQueries` flag
    private fun queryIndexInSmartMode(project: Project, query: (Project) -> Unit) {
        updateConfigs = false
        object : Task.Backgroundable(project, "mirrord") {
            override fun run(indicator: ProgressIndicator) {
                val dumbService = DumbService.getInstance(project)
                // refer to `DumbService`, there is no "guarantee" still, but we stay in a loop
                // todo: should probably look into using the message bus to listen for indexing to finish
                while (true) {
                    indicator.text = "mirrord: waiting for smart mode"
                    dumbService.waitForSmartMode()
                    val success = ReadAction.compute<Boolean, RuntimeException> {
                        if (project.isDisposed) {
                            throw ProcessCanceledException()
                        }
                        if (dumbService.isDumb) {
                            return@compute false
                        }
                        try {
                            indicator.text = "mirrord: updating config files"
                            query(project)
                            blockQueries = false
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
        }.queue()
    }

    // this function checks the `blockQueries` and `updateConfigs` flag and blocks queries
    // by laying down concurrent instructions in a manner that everytime updateConfigs is set
    // a query will follow
    private fun blockAndQueryIndex(project: Project) {
        if (updateConfigs && !blockQueries) {
            blockQueries = true
            queryIndexInSmartMode(project, ::updateConfigFiles)
        }
    }


    companion object {
        var chosenFile: String? = null

        @Volatile
        var updateConfigs: Boolean = true
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
                            MirrordConfigDropDown.updateConfigs = true
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