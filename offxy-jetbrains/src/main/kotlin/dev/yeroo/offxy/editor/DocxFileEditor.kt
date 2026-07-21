package dev.yeroo.offxy.editor

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.util.Alarm
import dev.yeroo.offxy.engine.ChicoryEngine
import dev.yeroo.offxy.engine.DocxEngine
import java.awt.BorderLayout
import java.awt.event.ComponentAdapter
import java.awt.event.ComponentEvent
import java.beans.PropertyChangeListener
import java.beans.PropertyChangeSupport
import javax.swing.JComponent
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.SwingConstants

/**
 * FileEditor shell for a `.docx` tab: owns the engine and the [EditorView],
 * loads the file, keeps wrap width in sync with the viewport, and surfaces
 * the engine's dirty flag as [isModified]. Editing replay arrives in Task 5;
 * save/undo wiring in Task 6.
 */
class DocxFileEditor(
    @Suppress("unused") private val project: Project,
    private val file: VirtualFile,
) : UserDataHolderBase(), FileEditor {
    private val engine: DocxEngine = ChicoryEngine()
    private val panel = JPanel(BorderLayout())
    private val changeSupport = PropertyChangeSupport(this)
    private val widthAlarm = Alarm(Alarm.ThreadToUse.SWING_THREAD, this)
    private var modified = false

    val view: EditorView?
    private var pipeline: EditPipeline? = null

    init {
        val bytes = file.contentsToByteArray()
        if (bytes.isNotEmpty() && engine.open(bytes)) {
            val v = EditorView(project) { rid -> engine.media(rid) }
            view = v
            panel.add(v.editor.component, BorderLayout.CENTER)
            Disposer.register(this, v)
            v.applyRender(ViewModel(engine.render()))
            val p = EditPipeline(engine, v) { json -> refreshFrom(json) }
            pipeline = p
            v.document.addDocumentListener(p, this)
            v.editor.component.addComponentListener(object : ComponentAdapter() {
                override fun componentResized(e: ComponentEvent) = scheduleWidthSync()
            })
            scheduleWidthSync()
        } else {
            view = null
            val message =
                if (bytes.isEmpty()) "“${file.name}” is empty — it isn't a Word document yet."
                else "Offxy could not read this .docx file."
            panel.add(JLabel(message, SwingConstants.CENTER), BorderLayout.CENTER)
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
        if (model.dirty != modified) {
            val old = modified
            modified = model.dirty
            // FileEditor.PROP_MODIFIED ("modified") — the constant's home varies
            // across platform versions; the wire value is stable.
            changeSupport.firePropertyChange("modified", old, modified)
        }
    }

    fun engine(): DocxEngine = engine

    /** Test hook: run any deferred edit reconciliation now. */
    fun flushEdits() {
        pipeline?.flush()
    }

    override fun getComponent(): JComponent = panel

    override fun getPreferredFocusedComponent(): JComponent? = view?.editor?.contentComponent

    override fun getName(): String = "Offxy"

    override fun getFile(): VirtualFile = file

    override fun setState(state: FileEditorState) {}

    override fun isModified(): Boolean = modified

    override fun isValid(): Boolean = file.isValid

    override fun addPropertyChangeListener(listener: PropertyChangeListener) =
        changeSupport.addPropertyChangeListener(listener)

    override fun removePropertyChangeListener(listener: PropertyChangeListener) =
        changeSupport.removePropertyChangeListener(listener)

    override fun dispose() {
        engine.close()
    }
}
