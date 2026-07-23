package dev.yeroo.offxy.editor

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.editor.event.DocumentEvent
import com.intellij.openapi.editor.event.DocumentListener
import dev.yeroo.offxy.engine.DocxEngine

/**
 * The one edit pathway: every native Document change — typing, backspace,
 * Enter, paste, cut, undo/redo — arrives here as a [DocumentEvent] and is
 * replayed into the engine as position-sync + edit commands. The engine's
 * returned view then reconciles the Document (deferred out of the event,
 * where Document writes are forbidden; coalesced across rapid events).
 *
 * Mapping uses the ViewModel current when the event fires (it describes the
 * pre-event document). The engine stays authoritative: any replay failure
 * falls back to a full fresh render at reconcile time.
 */
class EditPipeline(
    private val engine: DocxEngine,
    private val view: EditorView,
    private val onViewJson: (String) -> Unit,
) : DocumentListener {
    private var pendingJson: String? = null
    private var scheduled = false

    override fun documentChanged(event: DocumentEvent) {
        if (view.suppressListener) return
        val pre = view.currentView() ?: return
        val removed = event.oldFragment.toString()
        val inserted = event.newFragment.toString()
        if (removed.isEmpty() && inserted.isEmpty()) return

        var json: String? = null
        try {
            if (removed.isNotEmpty()) {
                val (l1, c1) = pre.offsetToGrid(event.offset)
                val (l2, c2) = pre.offsetToGrid(event.offset + removed.length)
                if (l1 != l2 || c1 != c2) {
                    engine.cmd("click\t$l1\t$c1\t0")
                    engine.cmd("click\t$l2\t$c2\t1")
                    json = engine.cmd("delete")
                }
            }
            if (inserted.isNotEmpty()) {
                if (removed.isEmpty()) {
                    val (l, c) = pre.offsetToGrid(event.offset)
                    engine.cmd("click\t$l\t$c\t0")
                }
                json = when {
                    inserted == "\n" -> engine.cmd("newline")
                    inserted.contains('\n') -> engine.cmd("paste\t$inserted")
                    else -> engine.cmd("insert\t$inserted")
                }
            }
        } catch (_: Throwable) {
            json = null
        }
        // No command produced a view (or replay failed): full resync.
        pendingJson = json ?: engine.render()
        schedule()
    }

    private fun schedule() {
        if (scheduled) return
        scheduled = true
        ApplicationManager.getApplication().invokeLater { flush() }
    }

    /** Apply the pending reconcile now (tests call this directly). */
    fun flush() {
        scheduled = false
        val json = pendingJson ?: return
        pendingJson = null
        onViewJson(json)
    }
}
