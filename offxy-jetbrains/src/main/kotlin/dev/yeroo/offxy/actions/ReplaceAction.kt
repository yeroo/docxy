package dev.yeroo.offxy.actions

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.ui.Messages
import dev.yeroo.offxy.editor.Formatting

/** Replace-all in the focused Offxy document (engine-side, one undo step). */
class ReplaceAction : AnAction("Offxy: Replace…") {
    override fun update(e: AnActionEvent) {
        e.presentation.isEnabled = activeOffxyEditor(e.project)?.view != null
    }

    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val editor = activeOffxyEditor(project) ?: return
        val find = Messages.showInputDialog(
            project, "Find what", "Offxy — Replace", null,
        )?.takeIf { it.isNotEmpty() } ?: return
        val with = Messages.showInputDialog(
            project, "Replace “$find” with", "Offxy — Replace", null,
        ) ?: return // cancelled; empty string is a valid "delete" replacement
        Formatting.run(project, editor, "replace\t$find\t$with")
    }
}
