package dev.yeroo.offxy.editor

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.command.undo.DocumentReference
import com.intellij.openapi.command.undo.DocumentReferenceManager
import com.intellij.openapi.command.undo.DocumentReferenceProvider
import com.intellij.openapi.fileEditor.FileDocumentManagerListener
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileContentChangeEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.util.Alarm
import dev.yeroo.offxy.engine.ChicoryEngine
import dev.yeroo.offxy.engine.DocxEngine
import java.awt.BorderLayout
import java.awt.event.ComponentAdapter
import java.awt.event.ComponentEvent
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
 * FileEditor shell for a `.docx` tab: owns the engine and the [EditorView],
 * loads the file (offering to mint a fresh document for empty files), keeps
 * wrap width in sync with the viewport, follows external disk changes, and
 * surfaces the engine's dirty flag as [isModified].
 */
class DocxFileEditor(
    private val project: Project,
    private val docxFile: VirtualFile,
) : UserDataHolderBase(), FileEditor, DocumentReferenceProvider {
    private val engine: DocxEngine = ChicoryEngine()
    private val panel = JPanel(BorderLayout())
    private val changeSupport = PropertyChangeSupport(this)
    private val widthAlarm = Alarm(Alarm.ThreadToUse.SWING_THREAD, this)
    private var modified = false
    private var savingToDisk = false

    var view: EditorView? = null
        private set
    private var pipeline: EditPipeline? = null

    var isDisposed = false
        private set

    init {
        val bytes = docxFile.contentsToByteArray()
        when {
            bytes.isEmpty() -> showEmptyState()
            !openInView(bytes) -> showMessage("Offxy could not read this .docx file.")
        }
        // Follow external disk changes while the tab is open.
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(VirtualFileManager.VFS_CHANGES, object : BulkFileListener {
                override fun after(events: List<VFileEvent>) {
                    if (savingToDisk || isDisposed) return
                    if (events.any { it is VFileContentChangeEvent && it.file == docxFile }) {
                        reloadFromDisk()
                    }
                }
            })
        // Save All / Ctrl+S also saves Offxy tabs.
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(FileDocumentManagerListener.TOPIC, object : FileDocumentManagerListener {
                override fun beforeAllDocumentsSaving() = saveNow()
            })
    }

    /** Open bytes into a live editor view (builds it on first use). */
    private fun openInView(bytes: ByteArray): Boolean {
        if (!engine.open(bytes)) return false
        val existing = view
        if (existing != null) {
            existing.applyRender(ViewModel(engine.render()))
            return true
        }
        val v = EditorView(project, { rid -> engine.media(rid) }, ::followLink)
        view = v
        panel.removeAll()
        panel.add(DocxToolbar.create(project, this), BorderLayout.NORTH)
        panel.add(v.editor.component, BorderLayout.CENTER)
        panel.revalidate()
        panel.repaint()
        Disposer.register(this, v)
        v.applyRender(ViewModel(engine.render()))
        val p = EditPipeline(engine, v) { json -> refreshFrom(json) }
        pipeline = p
        v.document.addDocumentListener(p, this)
        v.editor.component.addComponentListener(object : ComponentAdapter() {
            override fun componentResized(e: ComponentEvent) = scheduleWidthSync()
        })
        scheduleWidthSync()
        // Advertise on the agent control surface (docxy --mcp sees this tab).
        runCatching {
            val server = CtlBridge.start(project, this)
            Disposer.register(this, server)
        }
        return true
    }

    /** Empty file: offer to turn it into a real Word document in place. */
    private fun showEmptyState() {
        val box = JPanel()
        box.layout = BoxLayout(box, BoxLayout.Y_AXIS)
        val note = JLabel("“${docxFile.name}” is empty — it isn't a Word document yet.")
        note.alignmentX = 0.5f
        val button = JButton("Create new Word document")
        button.alignmentX = 0.5f
        button.addActionListener { createNewDocument() }
        box.add(Box.createVerticalGlue())
        box.add(note)
        box.add(Box.createVerticalStrut(8))
        box.add(button)
        box.add(Box.createVerticalGlue())
        panel.removeAll()
        panel.add(box, BorderLayout.CENTER)
        panel.revalidate()
    }

    /** Mint a fresh empty document over the (empty) file and open it. */
    fun createNewDocument() {
        val minted = ChicoryEngine.fromMarkdown("")
        savingToDisk = true
        try {
            WriteAction.run<RuntimeException> { docxFile.setBinaryContent(minted) }
        } finally {
            savingToDisk = false
        }
        if (!openInView(minted)) showMessage("Offxy could not create the document.")
    }

    /** Re-open the on-disk bytes. By default an unmodified tab follows the
     *  disk silently and a modified tab keeps its edits (last writer wins at
     *  the next save); `force` (the ctl `doc.reload` semantics) drops unsaved
     *  edits like the terminal app does. */
    fun reloadFromDisk(force: Boolean = false) {
        if ((modified && !force) || view == null) return
        val bytes = docxFile.contentsToByteArray()
        if (bytes.isNotEmpty()) {
            openInView(bytes)
            markModified(false)
        }
    }

    private fun showMessage(message: String) {
        panel.removeAll()
        panel.add(JLabel(message, SwingConstants.CENTER), BorderLayout.CENTER)
        panel.revalidate()
    }

    /** Write the engine's lossless save bytes back to the file. */
    fun saveNow() {
        if (view == null || !modified) return
        val bytes = engine.save()
        savingToDisk = true
        try {
            WriteAction.run<RuntimeException> { docxFile.setBinaryContent(bytes) }
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

    /** Wrap width follows the visible column count (min 20), debounced. */
    private fun scheduleWidthSync() {
        val v = view ?: return
        widthAlarm.cancelAllRequests()
        widthAlarm.addRequest({
            val width = v.editor.scrollingModel.visibleArea.width
            val cellW = com.intellij.openapi.editor.ex.util.EditorUtil.getPlainSpaceWidth(v.editor)
            if (width > 0 && cellW > 0) {
                val cols = (width / cellW - 1).coerceAtLeast(20)
                refreshFrom(engine.cmd("width\t$cols"))
            }
        }, 120)
    }

    /** Apply a fresh view JSON and propagate the dirty flag. */
    fun refreshFrom(viewJson: String) {
        val v = view ?: return
        val model = ViewModel(viewJson)
        v.reconcile(model)
        markModified(model.dirty)
    }

    fun engine(): DocxEngine = engine

    /** Test hook: run any deferred edit reconciliation now. */
    fun flushEdits() {
        pipeline?.flush()
    }

    /** Follow a rendered hyperlink: `#anchor` (TOC entry, cross-reference)
     *  jumps to the bookmark's block via the engine's `goto`; anything else
     *  opens externally. */
    fun followLink(href: String) {
        val v = view ?: return
        if (href.startsWith("#")) {
            refreshFrom(engine.cmd("goto\t${href.removePrefix("#")}"))
            val model = v.currentView() ?: return
            v.editor.caretModel.moveToOffset(model.gridToOffset(model.caretLine, model.caretCol))
            v.editor.scrollingModel.scrollToCaret(com.intellij.openapi.editor.ScrollType.CENTER)
        } else {
            com.intellij.ide.BrowserUtil.browse(href)
        }
    }

    override fun getComponent(): JComponent = panel

    override fun getPreferredFocusedComponent(): JComponent? = view?.editor?.contentComponent

    override fun getName(): String = "Offxy"

    override fun getFile(): VirtualFile = docxFile

    /** Routes platform undo/redo to this editor's edits: both the native text
     *  undo (standalone document) and the formatting snapshot undo (file). */
    override fun getDocumentReferences(): Collection<DocumentReference> {
        val mgr = DocumentReferenceManager.getInstance()
        return listOfNotNull(view?.document?.let { mgr.create(it) }, mgr.create(docxFile))
    }

    override fun setState(state: FileEditorState) {}

    override fun isModified(): Boolean = modified

    override fun isValid(): Boolean = docxFile.isValid

    override fun addPropertyChangeListener(listener: PropertyChangeListener) =
        changeSupport.addPropertyChangeListener(listener)

    override fun removePropertyChangeListener(listener: PropertyChangeListener) =
        changeSupport.removePropertyChangeListener(listener)

    override fun dispose() {
        isDisposed = true
        // Unsaved edits are written back on close (JetBrains auto-save spirit;
        // a confirmation prompt is a possible follow-up).
        runCatching { saveNow() }
        engine.close()
    }
}
