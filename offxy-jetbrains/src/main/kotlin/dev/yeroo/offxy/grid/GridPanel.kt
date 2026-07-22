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
