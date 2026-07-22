package dev.yeroo.offxy.grid

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.fileEditor.FileDocumentManagerListener
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileContentChangeEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import dev.yeroo.offxy.engine.GridEngine
import java.awt.BorderLayout
import java.beans.PropertyChangeListener
import java.beans.PropertyChangeSupport
import javax.swing.Box
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JComponent
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.SwingConstants

/**
 * FileEditor shell for an `.xlsx` tab: owns the [GridEngine] and the
 * [GridPanel], mirrors the docx editor's dirty/save/reload/empty-file flows.
 * The editing chrome (formula bar, sheet tabs, toolbar) mounts around the
 * grid; the ctl bridge attaches per tab.
 */
class XlsxFileEditor(
    @Suppress("unused") private val project: Project,
    private val xlsxFile: VirtualFile,
) : UserDataHolderBase(), FileEditor {
    private val engine = GridEngine()
    private val panel = JPanel(BorderLayout())
    private val changeSupport = PropertyChangeSupport(this)
    private var modified = false
    private var savingToDisk = false

    var grid: GridPanel? = null
        private set

    var isDisposed = false
        private set

    init {
        val bytes = xlsxFile.contentsToByteArray()
        when {
            bytes.isEmpty() -> showEmptyState()
            !openInPanel(bytes) -> showMessage("Offxy could not read this .xlsx file.")
        }
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(VirtualFileManager.VFS_CHANGES, object : BulkFileListener {
                override fun after(events: List<VFileEvent>) {
                    if (savingToDisk || isDisposed) return
                    if (events.any { it is VFileContentChangeEvent && it.file == xlsxFile }) {
                        reloadFromDisk()
                    }
                }
            })
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(FileDocumentManagerListener.TOPIC, object : FileDocumentManagerListener {
                override fun beforeAllDocumentsSaving() = saveNow()
            })
    }

    private fun openInPanel(bytes: ByteArray): Boolean {
        if (!engine.open(bytes)) return false
        val existing = grid
        if (existing != null) {
            existing.requestWindow(0, 0)
            return true
        }
        val g = GridPanel(engine) { v -> markModified(v.dirty) }
        grid = g
        panel.removeAll()
        panel.add(g.scrollPane, BorderLayout.CENTER)
        panel.revalidate()
        panel.repaint()
        return true
    }

    private fun showEmptyState() {
        val box = JPanel()
        box.layout = BoxLayout(box, BoxLayout.Y_AXIS)
        val note = JLabel("“${xlsxFile.name}” is empty — it isn't an Excel workbook yet.")
        note.alignmentX = 0.5f
        val button = JButton("Create new workbook")
        button.alignmentX = 0.5f
        button.addActionListener { createNewWorkbook() }
        box.add(Box.createVerticalGlue())
        box.add(note)
        box.add(Box.createVerticalStrut(8))
        box.add(button)
        box.add(Box.createVerticalGlue())
        panel.removeAll()
        panel.add(box, BorderLayout.CENTER)
        panel.revalidate()
    }

    fun createNewWorkbook() {
        val minted = GridEngine.newWorkbook()
        savingToDisk = true
        try {
            WriteAction.run<RuntimeException> { xlsxFile.setBinaryContent(minted) }
        } finally {
            savingToDisk = false
        }
        if (!openInPanel(minted)) showMessage("Offxy could not create the workbook.")
    }

    fun reloadFromDisk(force: Boolean = false) {
        if ((modified && !force) || grid == null) return
        val bytes = xlsxFile.contentsToByteArray()
        if (bytes.isNotEmpty()) {
            openInPanel(bytes)
            markModified(false)
        }
    }

    private fun showMessage(message: String) {
        panel.removeAll()
        panel.add(JLabel(message, SwingConstants.CENTER), BorderLayout.CENTER)
        panel.revalidate()
    }

    fun saveNow() {
        if (grid == null || !modified) return
        val bytes = engine.save()
        savingToDisk = true
        try {
            WriteAction.run<RuntimeException> { xlsxFile.setBinaryContent(bytes) }
        } finally {
            savingToDisk = false
        }
        markModified(false)
    }

    fun markModified(value: Boolean) {
        if (value != modified) {
            val old = modified
            modified = value
            changeSupport.firePropertyChange("modified", old, modified)
        }
    }

    fun engine(): GridEngine = engine

    override fun getComponent(): JComponent = panel

    override fun getPreferredFocusedComponent(): JComponent? = grid?.table

    override fun getName(): String = "Offxy"

    override fun getFile(): VirtualFile = xlsxFile

    override fun setState(state: FileEditorState) {}

    override fun isModified(): Boolean = modified

    override fun isValid(): Boolean = xlsxFile.isValid

    override fun addPropertyChangeListener(listener: PropertyChangeListener) =
        changeSupport.addPropertyChangeListener(listener)

    override fun removePropertyChangeListener(listener: PropertyChangeListener) =
        changeSupport.removePropertyChangeListener(listener)

    override fun dispose() {
        isDisposed = true
        runCatching { saveNow() }
        engine.close()
    }
}
