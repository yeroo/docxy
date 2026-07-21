package dev.yeroo.offxy.editor

/** One styled run inside a rendered line. */
data class Span(
    val text: String,
    val bold: Boolean = false,
    val italic: Boolean = false,
    val underline: Boolean = false,
    val strike: Boolean = false,
    val dim: Boolean = false,
    val highlight: Boolean = false,
    val color: String? = null,
    val link: String? = null,
)

/** An image placeholder box in grid cells. */
data class ImageBox(
    val rid: String,
    val row: Int,
    val col: Int,
    val w: Int,
    val h: Int,
    val bordered: Boolean,
    val label: String,
)

/**
 * Parsed engine view JSON (see `docxwasm::Session::view_json`): the rendered
 * grid as text + styles, the caret, dirty flag, image boxes, and the per-line
 * editable segment column ranges (`segs`) hosts use for edit guards.
 *
 * Grid ↔ text mapping note: columns are treated as char indexes, which holds
 * for single-cell-width chars; double-width (CJK) columns drift and are
 * corrected by engine-authoritative reconciliation (known v1 limit).
 */
class ViewModel(json: String) {
    val lines: List<List<Span>>
    val caretLine: Int
    val caretCol: Int
    val dirty: Boolean
    val images: List<ImageBox>
    /** Per line: editable column ranges, start inclusive / end exclusive. */
    val segs: List<List<IntRange>>
    /** The whole grid as document text (lines joined with \n). */
    val text: String
    private val lineStarts: IntArray

    init {
        @Suppress("UNCHECKED_CAST")
        val root = Json.parse(json) as Map<String, Any?>

        lines = (root["lines"] as List<Any?>).map { line ->
            (line as List<Any?>).map { sp ->
                @Suppress("UNCHECKED_CAST")
                val m = sp as Map<String, Any?>
                Span(
                    text = m["t"] as? String ?: "",
                    bold = m["b"] != null,
                    italic = m["i"] != null,
                    underline = m["u"] != null,
                    strike = m["s"] != null,
                    dim = m["d"] != null,
                    highlight = m["h"] != null,
                    color = m["c"] as? String,
                    link = m["lnk"] as? String,
                )
            }
        }
        @Suppress("UNCHECKED_CAST")
        val caret = root["caret"] as Map<String, Any?>
        caretLine = (caret["line"] as Long).toInt()
        caretCol = (caret["col"] as Long).toInt()
        dirty = root["dirty"] == true
        images = (root["images"] as? List<Any?> ?: emptyList()).map {
            @Suppress("UNCHECKED_CAST")
            val m = it as Map<String, Any?>
            ImageBox(
                rid = m["rid"] as? String ?: "",
                row = (m["row"] as Long).toInt(),
                col = (m["col"] as Long).toInt(),
                w = (m["w"] as Long).toInt(),
                h = (m["h"] as Long).toInt(),
                bordered = m["bordered"] == 1L,
                label = m["label"] as? String ?: "",
            )
        }
        segs = (root["segs"] as? List<Any?> ?: emptyList()).map { line ->
            (line as List<Any?>).map { pair ->
                val p = pair as List<Any?>
                (p[0] as Long).toInt() until (p[1] as Long).toInt()
            }
        }

        val sb = StringBuilder()
        lineStarts = IntArray(lines.size)
        for ((i, line) in lines.withIndex()) {
            if (i > 0) sb.append('\n')
            lineStarts[i] = sb.length
            for (sp in line) sb.append(sp.text)
        }
        text = sb.toString()
    }

    fun lineCount(): Int = lines.size

    fun lineStart(line: Int): Int = lineStarts[line.coerceIn(0, lineStarts.size - 1)]

    private fun lineLength(line: Int): Int {
        val start = lineStarts[line]
        val end = if (line + 1 < lineStarts.size) lineStarts[line + 1] - 1 else text.length
        return end - start
    }

    /** Document offset → grid (line, col). Offsets on a `\n` map to line end. */
    fun offsetToGrid(offset: Int): Pair<Int, Int> {
        val off = offset.coerceIn(0, text.length)
        var line = lineStarts.indexOfLast { it <= off }
        if (line < 0) line = 0
        return line to (off - lineStarts[line]).coerceAtMost(lineLength(line))
    }

    /** Grid (line, col) → document offset, clamped into the line. */
    fun gridToOffset(line: Int, col: Int): Int {
        val l = line.coerceIn(0, lines.size - 1)
        return lineStarts[l] + col.coerceIn(0, lineLength(l))
    }

    /** Styled runs as absolute document-offset ranges (for highlighters). */
    fun styledRanges(): List<Pair<IntRange, Span>> {
        val out = ArrayList<Pair<IntRange, Span>>()
        for ((i, line) in lines.withIndex()) {
            var col = 0
            for (sp in line) {
                val start = lineStarts[i] + col
                col += sp.text.length
                if (sp.bold || sp.italic || sp.underline || sp.strike || sp.dim ||
                    sp.color != null || sp.link != null
                ) {
                    out.add(start until (lineStarts[i] + col) to sp)
                }
            }
        }
        return out
    }

    /**
     * Absolute offset ranges that are NOT editable model text — the per-line
     * complement of [segs] (list markers, table borders, image rows). Line
     * separators are deliberately unguarded: removals across them are replayed
     * as engine selections and reconciled.
     */
    fun guardRanges(): List<IntRange> {
        val out = ArrayList<IntRange>()
        for (i in lines.indices) {
            val len = lineLength(i)
            if (len == 0) continue
            val lineSegs = segs.getOrNull(i) ?: emptyList()
            var col = 0
            for (seg in lineSegs.sortedBy { it.first }) {
                if (seg.first > col) out.add(absRange(i, col, seg.first))
                col = maxOf(col, seg.last + 1)
            }
            if (col < len) out.add(absRange(i, col, len))
        }
        return out
    }

    private fun absRange(line: Int, colFrom: Int, colTo: Int): IntRange =
        (lineStarts[line] + colFrom) until (lineStarts[line] + colTo)
}

/** Minimal recursive-descent JSON parser (the view JSON is machine-built and
 *  well-formed; no external dependency wanted). Numbers parse as Long. */
internal object Json {
    fun parse(s: String): Any? = Cursor(s).value()

    /** Serialize maps/lists/strings/numbers/booleans/null back to JSON. */
    fun write(v: Any?): String = StringBuilder().also { emit(v, it) }.toString()

    private fun emit(v: Any?, sb: StringBuilder) {
        when (v) {
            null -> sb.append("null")
            is String -> quote(v, sb)
            is Boolean -> sb.append(if (v) "true" else "false")
            is Int, is Long -> sb.append(v.toString())
            is Number -> sb.append(v.toString())
            is Map<*, *> -> {
                sb.append('{')
                var first = true
                for ((k, value) in v) {
                    if (!first) sb.append(',')
                    first = false
                    quote(k.toString(), sb)
                    sb.append(':')
                    emit(value, sb)
                }
                sb.append('}')
            }
            is List<*> -> {
                sb.append('[')
                for ((i, item) in v.withIndex()) {
                    if (i > 0) sb.append(',')
                    emit(item, sb)
                }
                sb.append(']')
            }
            else -> quote(v.toString(), sb)
        }
    }

    private fun quote(s: String, sb: StringBuilder) {
        sb.append('"')
        for (c in s) {
            when {
                c == '"' -> sb.append("\\\"")
                c == '\\' -> sb.append("\\\\")
                c == '\n' -> sb.append("\\n")
                c == '\r' -> sb.append("\\r")
                c == '\t' -> sb.append("\\t")
                c.code < 0x20 -> sb.append("\\u%04x".format(c.code))
                else -> sb.append(c)
            }
        }
        sb.append('"')
    }

    private class Cursor(val s: String) {
        var i = 0

        fun value(): Any? {
            ws()
            return when (s[i]) {
                '{' -> obj()
                '[' -> arr()
                '"' -> str()
                't' -> lit("true", true)
                'f' -> lit("false", false)
                'n' -> lit("null", null)
                else -> num()
            }
        }

        private fun ws() {
            while (i < s.length && s[i].isWhitespace()) i++
        }

        private fun lit(word: String, v: Any?): Any? {
            i += word.length
            return v
        }

        private fun obj(): Map<String, Any?> {
            val m = LinkedHashMap<String, Any?>()
            i++ // {
            ws()
            if (s[i] == '}') { i++; return m }
            while (true) {
                ws()
                val k = str()
                ws(); i++ // :
                m[k] = value()
                ws()
                if (s[i] == ',') { i++; continue }
                i++ // }
                return m
            }
        }

        private fun arr(): List<Any?> {
            val a = ArrayList<Any?>()
            i++ // [
            ws()
            if (s[i] == ']') { i++; return a }
            while (true) {
                a.add(value())
                ws()
                if (s[i] == ',') { i++; continue }
                i++ // ]
                return a
            }
        }

        private fun str(): String {
            val sb = StringBuilder()
            i++ // "
            while (s[i] != '"') {
                if (s[i] == '\\') {
                    i++
                    when (val c = s[i]) {
                        'n' -> sb.append('\n'); 't' -> sb.append('\t')
                        'r' -> sb.append('\r'); 'b' -> sb.append('\b')
                        'f' -> sb.append(12.toChar())
                        'u' -> { sb.append(s.substring(i + 1, i + 5).toInt(16).toChar()); i += 4 }
                        else -> sb.append(c)
                    }
                } else sb.append(s[i])
                i++
            }
            i++ // "
            return sb.toString()
        }

        private fun num(): Long {
            val start = i
            if (s[i] == '-') i++
            while (i < s.length && s[i].isDigit()) i++
            return s.substring(start, i).toLong()
        }
    }
}
