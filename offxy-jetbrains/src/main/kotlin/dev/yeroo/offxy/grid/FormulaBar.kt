package dev.yeroo.offxy.grid

import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBTextField
import com.intellij.util.ui.JBUI
import java.awt.BorderLayout
import javax.swing.JPanel

/** Ref box + editable formula field: two faces of one editing state with the
 *  in-cell editor. Commits `set` at the active cell on Enter. */
class FormulaBar(
    private val commit: (row: Int, col: Int, text: String) -> Unit,
    private val focusGrid: () -> Unit,
) {
    private val ref = JBLabel("A1").apply {
        border = JBUI.Borders.empty(0, 8)
        preferredSize = java.awt.Dimension(JBUI.scale(64), preferredSize.height)
    }
    private val field = JBTextField()
    private var curR = 0
    private var curC = 0

    val component: JPanel = JPanel(BorderLayout()).apply {
        border = JBUI.Borders.customLineBottom(JBUI.CurrentTheme.CustomFrameDecorations.separatorForeground())
        add(ref, BorderLayout.WEST)
        add(field, BorderLayout.CENTER)
    }

    init {
        field.addActionListener {
            commit(curR, curC, field.text)
            focusGrid()
        }
    }

    fun update(view: GridViewModel) {
        curR = view.curR
        curC = view.curC
        ref.text = view.curRef
        if (!field.hasFocus()) {
            field.text = view.curSrc
        }
    }
}
