package dev.yeroo.offxy.editor

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.Separator
import com.intellij.openapi.project.Project
import javax.swing.JComponent

/** The webview's floating-toolbar button set as a real ActionToolbar. */
object DocxToolbar {
    fun create(project: Project, editor: DocxFileEditor): JComponent {
        val group = DefaultActionGroup()
        val buttons = listOf(
            Triple("B", "Bold", "bold"),
            Triple("I", "Italic", "italic"),
            Triple("U", "Underline", "underline"),
            Triple("S", "Strikethrough", "strike"),
            null,
            Triple("H1", "Heading 1", "heading\t1"),
            Triple("H2", "Heading 2", "heading\t2"),
            Triple("¶", "Normal style", "heading\t0"),
            null,
            Triple("•", "Bulleted list", "list\tbullet"),
            Triple("1.", "Numbered list", "list\tnumber"),
            null,
            Triple("⯇", "Align left", "align\tleft"),
            Triple("≡", "Center", "align\tcenter"),
            Triple("⯈", "Align right", "align\tright"),
            null,
            Triple("A−", "Smaller font", "fontsize\t-2"),
            Triple("A+", "Larger font", "fontsize\t2"),
        )
        for (b in buttons) {
            if (b == null) {
                group.add(Separator.getInstance())
                continue
            }
            val (label, title, cmd) = b
            group.add(object : AnAction(label, title, null) {
                override fun actionPerformed(e: AnActionEvent) {
                    Formatting.run(project, editor, cmd)
                    editor.view?.editor?.contentComponent?.requestFocusInWindow()
                }

                override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

                // No icons — the labels (B, I, H1, …) ARE the buttons. Without
                // this, icon-less toolbar actions render as empty squares.
                override fun displayTextInToolbar(): Boolean = true
            })
        }
        val toolbar = ActionManager.getInstance()
            .createActionToolbar("OffxyDocxToolbar", group, true)
        editor.view?.editor?.component?.let { toolbar.targetComponent = it }
        return toolbar.component
    }
}
