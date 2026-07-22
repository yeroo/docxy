package dev.yeroo.offxy.grid

import dev.yeroo.offxy.editor.Json

/** One non-blank cell in the served window. */
data class GridCell(
    val text: String,
    val align: Char?,      // 'r' | 'c' | null (left/general-text)
    val bold: Boolean,
    val italic: Boolean,
    val color: String?,    // "#rrggbb"
    val bg: String?,       // "#rrggbb"
)

/**
 * Parsed gridwasm viewport JSON (see `gridwasm::Session::view_json`): sheet
 * list, used extent, visible column widths, the window's non-blank cells,
 * selection, active-cell state, dirty flag, the `edits` mutation counter,
 * and the one-shot `err`/`copied` fields.
 *
 * The window coordinates themselves are NOT echoed — the host remembers what
 * it requested.
 */
class GridViewModel(json: String) {
    val sheets: List<String>
    val active: Int
    val rows: Int
    val cols: Int
    val colWidths: Map<Int, Double>
    val cells: Map<Pair<Int, Int>, GridCell>
    val selR: Int; val selC: Int; val selR2: Int; val selC2: Int
    val curRef: String
    val curSrc: String
    val curBold: Boolean
    val curItalic: Boolean
    val curAlign: Char
    val dirty: Boolean
    val edits: Long
    val err: String?
    val copied: String?

    init {
        @Suppress("UNCHECKED_CAST")
        val root = Json.parse(json) as Map<String, Any?>
        sheets = (root["sheets"] as List<Any?>).map { it as String }
        active = (root["active"] as Long).toInt()

        @Suppress("UNCHECKED_CAST")
        val dims = root["dims"] as Map<String, Any?>
        rows = (dims["rows"] as Long).toInt()
        cols = (dims["cols"] as Long).toInt()

        colWidths = (root["colw"] as? List<Any?> ?: emptyList()).associate {
            @Suppress("UNCHECKED_CAST")
            val m = it as Map<String, Any?>
            (m["c"] as Long).toInt() to (m["w"] as? Double ?: (m["w"] as Long).toDouble())
        }

        cells = (root["cells"] as? List<Any?> ?: emptyList()).associate {
            @Suppress("UNCHECKED_CAST")
            val m = it as Map<String, Any?>
            val key = (m["r"] as Long).toInt() to (m["c"] as Long).toInt()
            key to GridCell(
                text = m["t"] as? String ?: "",
                align = (m["a"] as? String)?.firstOrNull(),
                bold = m["b"] != null,
                italic = m["i"] != null,
                color = m["col"] as? String,
                bg = m["bg"] as? String,
            )
        }

        @Suppress("UNCHECKED_CAST")
        val sel = root["sel"] as Map<String, Any?>
        selR = (sel["r"] as Long).toInt()
        selC = (sel["c"] as Long).toInt()
        selR2 = (sel["r2"] as Long).toInt()
        selC2 = (sel["c2"] as Long).toInt()

        @Suppress("UNCHECKED_CAST")
        val cur = root["cur"] as Map<String, Any?>
        curRef = cur["ref"] as? String ?: ""
        curSrc = cur["src"] as? String ?: ""
        curBold = cur["bold"] == true
        curItalic = cur["italic"] == true
        curAlign = (cur["align"] as? String)?.firstOrNull() ?: 'g'

        dirty = root["dirty"] == true
        edits = root["edits"] as? Long ?: 0L
        err = root["err"] as? String
        copied = root["copied"] as? String
    }

    companion object {
        /** 0-based column index → A1-style letters (0 = A, 26 = AA). */
        fun columnName(c: Int): String {
            var n = c
            val sb = StringBuilder()
            while (n >= 0) {
                sb.insert(0, ('A' + n % 26))
                n = n / 26 - 1
            }
            return sb.toString()
        }
    }
}
