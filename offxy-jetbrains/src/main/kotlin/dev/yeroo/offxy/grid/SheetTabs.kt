package dev.yeroo.offxy.grid

import com.intellij.openapi.ui.Messages
import com.intellij.util.ui.JBUI
import java.awt.FlowLayout
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.JButton
import javax.swing.JPanel
import javax.swing.JToggleButton

/** Bottom sheet strip: click switches, `+` adds, double-click renames. */
class SheetTabs(
    private val onCommand: (String) -> Unit,     // non-mutating (switch)
    private val onMutate: (String) -> Unit,      // mutating (add/rename)
) {
    val component: JPanel = JPanel(FlowLayout(FlowLayout.LEFT, JBUI.scale(2), JBUI.scale(1))).apply {
        border = JBUI.Borders.customLineTop(JBUI.CurrentTheme.CustomFrameDecorations.separatorForeground())
    }

    fun update(view: GridViewModel) {
        component.removeAll()
        for ((i, name) in view.sheets.withIndex()) {
            val tab = JToggleButton(name, i == view.active)
            tab.isFocusable = false
            tab.addMouseListener(object : MouseAdapter() {
                override fun mouseClicked(e: MouseEvent) {
                    if (e.clickCount >= 2) {
                        val newName = Messages.showInputDialog(
                            component, "Rename sheet “$name”", "Offxy — Rename Sheet", null, name, null,
                        )?.takeIf { it.isNotBlank() && it != name } ?: return
                        onMutate("sheet\trename\t$i\t$newName")
                    } else if (i != view.active) {
                        onCommand("sheet\tswitch\t$i")
                    }
                }
            })
            component.add(tab)
        }
        val add = JButton("+")
        add.isFocusable = false
        add.toolTipText = "Add sheet"
        add.addActionListener {
            onMutate("sheet\tadd\tSheet${view.sheets.size + 1}")
        }
        component.add(add)
        component.revalidate()
        component.repaint()
    }
}
