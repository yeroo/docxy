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
    private val project: Project,
    private val xlsxFile: VirtualFile,
) : UserDataHolderBase(), FileEditor,
    com.intellij.openapi.command.undo.DocumentReferenceProvider {

    /** Routes platform undo/redo to this editor (no Document — file ref only). */
    override fun getDocumentReferences(): Collection<com.intellij.openapi.command.undo.DocumentReference> =
        listOf(com.intellij.openapi.command.undo.DocumentReferenceManager.getInstance().create(xlsxFile))

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

    private var formulaBar: FormulaBar? = null

    private fun openInPanel(bytes: ByteArray): Boolean {
        if (!engine.open(bytes)) return false
        val existing = grid
        if (existing != null) {
            existing.requestWindow(0, 0)
            return true
        }
        val bar = FormulaBar(
            commit = { r, c, text -> runMutating("set\t$r\t$c\t$text") },
            focusGrid = { grid?.table?.requestFocusInWindow() },
        )
        formulaBar = bar
        val g = GridPanel(
            engine,
            onView = { v ->
                bar.update(v)
                sheetTabs?.update(v)
                markModified(v.dirty)
            },
            onMutate = { command -> runMutating(command) },
        )
        grid = g
        val tabs = SheetTabs(
            onCommand = { c ->
                g.cmd(c)
                g.requestWindow(0, 0)
            },
            onMutate = { c -> runMutating(c) },
        )
        sheetTabs = tabs
        tabs.update(g.view)

        val north = JPanel(BorderLayout())
        north.add(bar.component, BorderLayout.NORTH)
        north.add(GridToolbar.create(this), BorderLayout.SOUTH)

        panel.removeAll()
        panel.add(north, BorderLayout.NORTH)
        panel.add(g.scrollPane, BorderLayout.CENTER)
        panel.add(tabs.component, BorderLayout.SOUTH)
        panel.revalidate()
        panel.repaint()
        installContextMenu(g)
        // Advertise on the agent control surface (xlsxy --mcp sees this tab).
        runCatching {
            val server = GridCtlBridge.start(project, this)
            com.intellij.openapi.util.Disposer.register(this, server)
        }
        return true
    }

    /** Ctl-side undo registration — same engine-stack step as UI edits. */
    fun registerAgentUndo(project: Project, verb: String) {
        com.intellij.openapi.command.CommandProcessor.getInstance().executeCommand(project, {
            com.intellij.openapi.command.undo.UndoManager.getInstance(project)
                .undoableActionPerformed(GridUndo(this))
        }, "Offxy agent: $verb", null)
    }

    private var sheetTabs: SheetTabs? = null

    /** Right-click: structural edits at the selection. */
    private fun installContextMenu(g: GridPanel) {
        val menu = javax.swing.JPopupMenu()
        fun item(label: String, command: (IntArray) -> String) {
            val mi = javax.swing.JMenuItem(label)
            mi.addActionListener {
                val rows = g.table.selectedRows
                val cols = g.table.selectedColumns
                if (rows.isEmpty() || cols.isEmpty()) return@addActionListener
                val b = intArrayOf(rows.min(), cols.min(), rows.max(), cols.max())
                runMutating(command(b))
            }
            menu.add(mi)
        }
        item("Insert rows above") { b -> "insrow\t${b[0]}\t${b[2] - b[0] + 1}" }
        item("Delete rows") { b -> "delrow\t${b[0]}\t${b[2] - b[0] + 1}" }
        menu.addSeparator()
        item("Insert columns left") { b -> "inscol\t${b[1]}\t${b[3] - b[1] + 1}" }
        item("Delete columns") { b -> "delcol\t${b[1]}\t${b[3] - b[1] + 1}" }
        g.table.componentPopupMenu = menu
    }

    /** One mutating command = one platform undo step driving the engine's own
     *  undo stack (no snapshots — every grid mutation is transactional). */
    fun runMutating(command: String) {
        val g = grid ?: return
        val before = g.view.edits
        val v = g.cmd(command)
        v.copied?.let { tsv ->
            com.intellij.openapi.ide.CopyPasteManager.getInstance()
                .setContents(java.awt.datatransfer.StringSelection(tsv))
        }
        v.err?.let { showError(it) }
        if (v.edits != before) {
            com.intellij.openapi.command.CommandProcessor.getInstance().executeCommand(project, {
                com.intellij.openapi.command.undo.UndoManager.getInstance(project)
                    .undoableActionPerformed(GridUndo(this))
            }, "Offxy: $command", null)
        }
    }

    private fun showError(message: String) {
        val table = grid?.table ?: return
        com.intellij.openapi.ui.popup.JBPopupFactory.getInstance()
            .createHtmlTextBalloonBuilder(message, com.intellij.openapi.ui.MessageType.WARNING, null)
            .setFadeoutTime(4000)
            .createBalloon()
            .show(
                com.intellij.ui.awt.RelativePoint.getNorthWestOf(table),
                com.intellij.openapi.ui.popup.Balloon.Position.above,
            )
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

/** Undo/redo drive the engine's own stack — the tab's single edit source. */
private class GridUndo(
    private val editor: XlsxFileEditor,
) : com.intellij.openapi.command.undo.BasicUndoableAction(
    com.intellij.openapi.command.undo.DocumentReferenceManager.getInstance().create(editor.file),
) {
    override fun undo() {
        if (!editor.isDisposed) editor.grid?.cmd("undo")
    }

    override fun redo() {
        if (!editor.isDisposed) editor.grid?.cmd("redo")
    }
}
