package dev.yeroo.offxy.grid

import com.intellij.ui.table.JBTable
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.UIUtil
import dev.yeroo.offxy.engine.GridEngine
import java.awt.Color
import java.awt.Component
import java.awt.Font
import javax.swing.JScrollPane
import javax.swing.JTable
import javax.swing.ListSelectionModel
import javax.swing.table.AbstractTableModel
import javax.swing.table.DefaultTableCellRenderer
import javax.swing.table.JTableHeader

/**
 * The virtualized grid: a JBTable whose model reads from the last served
 * viewport window; scrolling re-requests the window (with margin) from the
 * engine. Cell display text arrives pre-formatted (numfmt applied); styles
 * (alignment/bold/italic/colors) come per cell from the view JSON.
 *
 * All engine access on the EDT.
 */
class GridPanel(
    private val engine: GridEngine,
    private val onView: (GridViewModel) -> Unit,
    private val onMutate: (String) -> Unit = { },
) {
    /** Grid extent shown beyond the used range, so there is room to grow. */
    private val marginRows = 40
    private val marginCols = 8

    var view: GridViewModel = GridViewModel(engine.cmd("view\t0\t0\t0\t$WINDOW_ROWS\t$WINDOW_COLS"))
        private set

    private var windowTop = 0
    private var windowLeft = 0

    val model = object : AbstractTableModel() {
        override fun getRowCount(): Int = maxOf(view.rows + marginRows, 60)
        override fun getColumnCount(): Int = maxOf(view.cols + marginCols, 26)
        override fun getValueAt(row: Int, col: Int): Any? = view.cells[row to col]
        override fun getColumnName(col: Int): String = GridViewModel.columnName(col)
        override fun isCellEditable(row: Int, col: Int): Boolean = true

        /** The commit path — the in-place editor and tests both land here. */
        override fun setValueAt(value: Any?, row: Int, col: Int) {
            onMutate("set\t$row\t$col\t${value ?: ""}")
        }
    }

    val table: JBTable = object : JBTable(model) {
        override fun getCellRenderer(row: Int, column: Int) = cellRenderer
    }

    val scrollPane: JScrollPane

    private val cellRenderer = object : DefaultTableCellRenderer() {
        override fun getTableCellRendererComponent(
            tbl: JTable, value: Any?, isSelected: Boolean, hasFocus: Boolean, row: Int, column: Int,
        ): Component {
            val comp = super.getTableCellRendererComponent(tbl, "", isSelected, hasFocus, row, column)
            val cell = value as? GridCell
            text = cell?.text ?: ""
            horizontalAlignment = when (cell?.align) {
                'r' -> RIGHT
                'c' -> CENTER
                else -> LEFT
            }
            var style = Font.PLAIN
            if (cell?.bold == true) style = style or Font.BOLD
            if (cell?.italic == true) style = style or Font.ITALIC
            font = tbl.font.deriveFont(style)
            if (!isSelected) {
                foreground = cell?.color?.let(::hex) ?: UIUtil.getTableForeground()
                background = cell?.bg?.let(::hex) ?: UIUtil.getTableBackground()
            }
            return comp
        }
    }

    init {
        table.apply {
            autoResizeMode = JTable.AUTO_RESIZE_OFF
            setCellSelectionEnabled(true)
            selectionModel.selectionMode = ListSelectionModel.SINGLE_INTERVAL_SELECTION
            columnSelectionAllowed = true
            rowSelectionAllowed = true
            setShowGrid(true)
            gridColor = JBUI.CurrentTheme.CustomFrameDecorations.separatorForeground()
            putClientProperty("terminateEditOnFocusLost", true)
        }
        applyColumnWidths()
        scrollPane = JBUI.Panels.simplePanel().let { JScrollPane(table) }
        scrollPane.setRowHeaderView(rowHeader())
        scrollPane.viewport.addChangeListener { refreshWindowIfNeeded() }
        installEditing()
    }

    // ---- editing: selection sync, in-place editor, clipboard, delete --------

    private var syncing = false

    private fun installEditing() {
        // Selection follows into the engine (drives cur.ref/src for the bar).
        val selListener = javax.swing.event.ListSelectionListener { e ->
            if (e.valueIsAdjusting || syncing) return@ListSelectionListener
            val r = table.selectionModel.leadSelectionIndex
            val c = table.columnModel.selectionModel.leadSelectionIndex
            if (r < 0 || c < 0) return@ListSelectionListener
            val ar = table.selectionModel.anchorSelectionIndex.takeIf { it >= 0 } ?: r
            val ac = table.columnModel.selectionModel.anchorSelectionIndex.takeIf { it >= 0 } ?: c
            syncing = true
            try {
                cmd("select\t$r\t$c\t$ar\t$ac")
            } finally {
                syncing = false
            }
        }
        table.selectionModel.addListSelectionListener(selListener)
        table.columnModel.selectionModel.addListSelectionListener(selListener)

        // In-place editor: prefilled with the RAW source (formula, not the
        // rendered value), selected so type-through replaces.
        val field = javax.swing.JTextField()
        table.setDefaultEditor(Any::class.java, object : javax.swing.DefaultCellEditor(field) {
            override fun getTableCellEditorComponent(
                tbl: JTable, value: Any?, isSelected: Boolean, row: Int, col: Int,
            ): Component {
                super.getTableCellEditorComponent(tbl, "", isSelected, row, col)
                if (view.curR != row || view.curC != col) {
                    cmd("select\t$row\t$col")
                }
                field.text = view.curSrc
                field.selectAll()
                return field
            }

            override fun getCellEditorValue(): Any = field.text
        })

        fun selBounds(): IntArray? {
            val rows = table.selectedRows
            val cols = table.selectedColumns
            if (rows.isEmpty() || cols.isEmpty()) return null
            return intArrayOf(rows.min(), cols.min(), rows.max(), cols.max())
        }

        fun toClipboard(text: String) {
            com.intellij.openapi.ide.CopyPasteManager.getInstance()
                .setContents(java.awt.datatransfer.StringSelection(text))
        }

        val actionMap = table.actionMap
        actionMap.put("copy", object : javax.swing.AbstractAction() {
            override fun actionPerformed(e: java.awt.event.ActionEvent) {
                val b = selBounds() ?: return
                cmd("select\t${b[2]}\t${b[3]}\t${b[0]}\t${b[1]}")
                cmd("copy").copied?.let(::toClipboard)
            }
        })
        actionMap.put("cut", object : javax.swing.AbstractAction() {
            override fun actionPerformed(e: java.awt.event.ActionEvent) {
                val b = selBounds() ?: return
                cmd("select\t${b[2]}\t${b[3]}\t${b[0]}\t${b[1]}")
                view.copied // stale; the cut's TSV arrives with the mutation:
                onMutate("cut")
                view.copied?.let(::toClipboard)
            }
        })
        actionMap.put("paste", object : javax.swing.AbstractAction() {
            override fun actionPerformed(e: java.awt.event.ActionEvent) {
                val b = selBounds() ?: return
                val text = com.intellij.openapi.ide.CopyPasteManager.getInstance()
                    .getContents<String>(java.awt.datatransfer.DataFlavor.stringFlavor) ?: return
                onMutate("paste\t${b[0]}\t${b[1]}\t$text")
            }
        })
        table.inputMap.put(javax.swing.KeyStroke.getKeyStroke("DELETE"), "offxy.clear")
        actionMap.put("offxy.clear", object : javax.swing.AbstractAction() {
            override fun actionPerformed(e: java.awt.event.ActionEvent) {
                val b = selBounds() ?: return
                onMutate("clear\t${b[0]}\t${b[1]}\t${b[2]}\t${b[3]}")
            }
        })
    }

    /** Sticky numbered row header. */
    private fun rowHeader(): Component {
        val header = object : JBTable(object : AbstractTableModel() {
            override fun getRowCount(): Int = model.rowCount
            override fun getColumnCount(): Int = 1
            override fun getValueAt(r: Int, c: Int): Any = (r + 1).toString()
        }) {}
        header.preferredScrollableViewportSize = java.awt.Dimension(JBUI.scale(46), 0)
        header.rowHeight = table.rowHeight
        header.isFocusable = false
        header.setRowSelectionAllowed(false)
        val r = DefaultTableCellRenderer()
        r.horizontalAlignment = DefaultTableCellRenderer.CENTER
        r.background = UIUtil.getPanelBackground()
        header.setDefaultRenderer(Any::class.java, r)
        header.tableHeader = JTableHeader() // blank corner alignment
        return header
    }

    /** Visible range (plus margin) → engine window, when it moved. */
    private fun refreshWindowIfNeeded() {
        val rect = scrollPane.viewport.viewRect
        if (rect.height == 0) return
        val firstRow = (table.rowAtPoint(rect.location).takeIf { it >= 0 } ?: 0)
        val firstCol = (table.columnAtPoint(rect.location).takeIf { it >= 0 } ?: 0)
        val top = (firstRow - 10).coerceAtLeast(0)
        val left = (firstCol - 4).coerceAtLeast(0)
        if (top != windowTop || left != windowLeft) {
            requestWindow(top, left)
        }
    }

    fun requestWindow(top: Int, left: Int) {
        windowTop = top
        windowLeft = left
        applyView(GridViewModel(engine.cmd("view\t${view.active}\t$top\t$left\t$WINDOW_ROWS\t$WINDOW_COLS")))
    }

    /** Install a fresh view (any command's return), repaint, notify. */
    fun applyView(v: GridViewModel) {
        val extentChanged = v.rows != view.rows || v.cols != view.cols || v.sheets != view.sheets
        view = v
        if (extentChanged) model.fireTableStructureChanged() else model.fireTableRowsUpdated(0, model.rowCount - 1)
        applyColumnWidths()
        onView(v)
    }

    /** Run one engine command and install its returned view. */
    fun cmd(command: String): GridViewModel {
        val v = GridViewModel(engine.cmd(command))
        applyView(v)
        return v
    }

    private fun applyColumnWidths() {
        val charW = table.getFontMetrics(table.font).charWidth('0')
        for ((c, w) in view.colWidths) {
            if (c < table.columnModel.columnCount) {
                table.columnModel.getColumn(c).preferredWidth =
                    (w * charW).toInt().coerceIn(JBUI.scale(24), JBUI.scale(600))
            }
        }
    }

    private fun hex(s: String): Color? =
        runCatching { Color.decode(s) }.getOrNull()

    companion object {
        const val WINDOW_ROWS = 80
        const val WINDOW_COLS = 40
    }
}
