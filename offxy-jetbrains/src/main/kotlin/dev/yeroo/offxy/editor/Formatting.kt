package dev.yeroo.offxy.editor

import com.intellij.openapi.command.CommandProcessor
import com.intellij.openapi.command.undo.BasicUndoableAction
import com.intellij.openapi.command.undo.DocumentReferenceManager
import com.intellij.openapi.command.undo.UndoManager
import com.intellij.openapi.project.Project

/**
 * Engine-side commands that change more than text (bold, headings, lists,
 * alignment, font size, replace-all): sync the engine position/selection from
 * the editor, dispatch, reconcile, and register one snapshot-based undo step.
 *
 * Undo restores full engine save-bytes (before/after) — deliberately not the
 * engine's internal undo stack, which also records replayed text edits and
 * would interleave wrongly with the platform's Document undo.
 */
object Formatting {
    fun run(project: Project, editor: DocxFileEditor, command: String) {
        val v = editor.view ?: return
        val engine = editor.engine()
        val view = v.currentView() ?: return

        // Snapshot before (save() briefly clears the engine dirty flag; any
        // mutating command sets it again, and refreshFrom re-reads it).
        val before = engine.save()

        val sel = v.editor.selectionModel
        if (sel.hasSelection()) {
            val (l1, c1) = view.offsetToGrid(sel.selectionStart)
            val (l2, c2) = view.offsetToGrid(sel.selectionEnd)
            engine.cmd("click\t$l1\t$c1\t0")
            engine.cmd("click\t$l2\t$c2\t1")
        } else {
            val (l, c) = view.offsetToGrid(v.editor.caretModel.offset)
            engine.cmd("click\t$l\t$c\t0")
        }

        // Reconcile OUTSIDE the command so the document patch stays truly
        // undo-transparent — undo must revert via the snapshot only, never via
        // a competing document-level reverse patch (they fight at stale offsets).
        val json = engine.cmd(command)
        editor.refreshFrom(json)
        CommandProcessor.getInstance().executeCommand(project, {
            UndoManager.getInstance(project)
                .undoableActionPerformed(SnapshotUndo(editor, before))
        }, "Offxy: $command", null)
    }
}

/**
 * Before/after engine snapshots as one platform undo step. The redo bytes are
 * captured lazily at first undo (the engine is at the post-command state by
 * then — later edits have already been popped off the platform stack).
 */
private class SnapshotUndo(
    private val editor: DocxFileEditor,
    private val before: ByteArray,
) : BasicUndoableAction(
    DocumentReferenceManager.getInstance().create(editor.view!!.document),
    DocumentReferenceManager.getInstance().create(editor.file),
) {
    private var after: ByteArray? = null

    override fun undo() {
        if (after == null) after = editor.engine().save()
        restore(before)
    }

    override fun redo() {
        after?.let { restore(it) }
    }

    private fun restore(bytes: ByteArray) {
        val engine = editor.engine()
        if (engine.open(bytes)) {
            // A restored state may differ from disk; be conservative.
            editor.markModified(true)
            // Document writes are forbidden inside an undo transaction; the
            // engine is restored now, the surface reconciles right after.
            com.intellij.openapi.application.ApplicationManager.getApplication().invokeLater {
                if (!editor.isDisposed) {
                    editor.view?.applyRender(ViewModel(engine.render()))
                }
            }
        }
    }
}
