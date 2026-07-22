package dev.yeroo.offxy.grid

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.Separator
import javax.swing.JComponent

/** Formatting strip over the grid — labels as buttons, like the docx bar. */
object GridToolbar {
    fun create(editor: XlsxFileEditor): JComponent {
        val group = DefaultActionGroup()
        val buttons = listOf(
            Triple("B", "Bold", "fmt\tbold\ttoggle"),
            Triple("I", "Italic", "fmt\titalic\ttoggle"),
            null,
            Triple("⯇", "Align left", "fmt\talign\tl"),
            Triple("≡", "Center", "fmt\talign\tc"),
            Triple("⯈", "Align right", "fmt\talign\tr"),
            null,
            Triple(".0−", "Fewer decimals", "decimals\t-1"),
            Triple(".0+", "More decimals", "decimals\t1"),
            null,
            Triple("Σ", "Autosum", "autosum"),
        )
        for (b in buttons) {
            if (b == null) {
                group.add(Separator.getInstance())
                continue
            }
            val (label, title, cmd) = b
            group.add(object : AnAction(label, title, null) {
                override fun actionPerformed(e: AnActionEvent) {
                    editor.runMutating(cmd)
                    editor.grid?.table?.requestFocusInWindow()
                }

                override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

                override fun displayTextInToolbar(): Boolean = true
            })
        }
        val toolbar = ActionManager.getInstance().createActionToolbar("OffxyGridToolbar", group, true)
        editor.grid?.table?.let { toolbar.targetComponent = it }
        return toolbar.component
    }
}
