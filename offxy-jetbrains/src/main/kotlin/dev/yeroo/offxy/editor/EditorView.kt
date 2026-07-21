package dev.yeroo.offxy.editor

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.command.CommandProcessor
import com.intellij.openapi.editor.Document
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.editor.VisualPosition
import com.intellij.openapi.editor.colors.EditorColorsManager
import com.intellij.openapi.editor.ex.EditorEx
import com.intellij.openapi.editor.ex.util.EditorUtil
import com.intellij.openapi.editor.markup.EffectType
import com.intellij.openapi.editor.markup.HighlighterLayer
import com.intellij.openapi.editor.markup.HighlighterTargetArea
import com.intellij.openapi.editor.markup.RangeHighlighter
import com.intellij.openapi.editor.markup.TextAttributes
import com.intellij.openapi.project.Project
import com.intellij.openapi.Disposable
import com.intellij.ui.JBColor
import java.awt.Font
import java.awt.Graphics2D
import java.awt.Image
import java.awt.Rectangle
import java.awt.RenderingHints
import java.io.ByteArrayInputStream
import javax.imageio.ImageIO

/**
 * The IntelliJ editor as the document surface: an editable [Document] whose
 * text is the engine's grid render, styled with range highlighters, with
 * guarded blocks over decoration columns and image boxes painted by custom
 * renderers. The engine stays authoritative — [applyRender] replaces
 * everything, [reconcile] patches the minimal changed line range.
 *
 * All methods run on the EDT. [suppressListener] is raised around every
 * self-inflicted write so the edit pipeline (Task 5) ignores them.
 */
class EditorView(
    private val project: Project?,
    private val media: (String) -> ByteArray,
) : Disposable {
    val document: Document = EditorFactory.getInstance().createDocument("")
    val editor: EditorEx = EditorFactory.getInstance().createEditor(document, project) as EditorEx

    var suppressListener = false
        private set

    private var view: ViewModel? = null
    private val guardMarkers = ArrayList<com.intellij.openapi.editor.RangeMarker>()
    private val highlighters = ArrayList<RangeHighlighter>()
    private val imageCache = HashMap<String, Image?>()

    init {
        editor.settings.apply {
            isLineNumbersShown = false
            isIndentGuidesShown = false
            isFoldingOutlineShown = false
            isRightMarginShown = false
            isUseSoftWraps = false
            additionalLinesCount = 1
            isAdditionalPageAtBottom = false
        }
    }

    fun currentView(): ViewModel? = view

    /** Full apply: replace text, markup, guards, and image renderers. */
    fun applyRender(v: ViewModel) {
        selfWrite { document.setText(v.text) }
        this.view = v
        applyMarkup(v)
    }

    /**
     * Minimal patch: find the changed line window between the current document
     * text and the fresh render, replace only that region, then refresh markup.
     * No-op (beyond markup refresh) when the texts already agree.
     */
    fun reconcile(v: ViewModel) {
        val old = document.text
        if (old != v.text) {
            val oldLines = old.split('\n')
            val newLines = v.text.split('\n')
            var prefix = 0
            while (prefix < oldLines.size && prefix < newLines.size &&
                oldLines[prefix] == newLines[prefix]
            ) prefix++
            var suffix = 0
            while (suffix < oldLines.size - prefix && suffix < newLines.size - prefix &&
                oldLines[oldLines.size - 1 - suffix] == newLines[newLines.size - 1 - suffix]
            ) suffix++

            val startOff = oldLines.take(prefix).sumOf { it.length + 1 }
            val oldEnd = old.length - suffix(oldLines, suffix)
            val newEnd = v.text.length - suffix(newLines, suffix)
            selfWrite {
                document.replaceString(
                    startOff.coerceAtMost(old.length),
                    oldEnd.coerceIn(startOff, old.length),
                    v.text.substring(startOff.coerceAtMost(v.text.length), newEnd.coerceIn(startOff, v.text.length)),
                )
            }
        }
        this.view = v
        applyMarkup(v)
    }

    private fun suffix(lines: List<String>, n: Int): Int {
        var chars = 0
        for (k in 0 until n) chars += lines[lines.size - 1 - k].length + 1
        return if (n == 0) 0 else chars - if (n == lines.size) 1 else 0
    }

    private fun selfWrite(body: () -> Unit) {
        suppressListener = true
        try {
            CommandProcessor.getInstance().runUndoTransparentAction {
                WriteAction.run<RuntimeException> { body() }
            }
        } finally {
            suppressListener = false
        }
    }

    // ---- markup: highlighters, guards, images -------------------------------

    private fun applyMarkup(v: ViewModel) {
        val markup = editor.markupModel
        highlighters.forEach { it.dispose() }
        highlighters.clear()
        guardMarkers.forEach { document.removeGuardedBlock(it) }
        guardMarkers.clear()

        for ((range, span) in v.styledRanges()) {
            val attrs = TextAttributes().apply {
                var font = Font.PLAIN
                if (span.bold) font = font or Font.BOLD
                if (span.italic) font = font or Font.ITALIC
                fontType = font
                if (span.underline || span.link != null) {
                    effectType = EffectType.LINE_UNDERSCORE
                    effectColor = if (span.link != null) LINK else fg(span)
                } else if (span.strike) {
                    effectType = EffectType.STRIKEOUT
                    effectColor = fg(span)
                }
                foregroundColor = fg(span)
            }
            highlighters.add(
                markup.addRangeHighlighter(
                    range.first, range.last + 1,
                    HighlighterLayer.SYNTAX, attrs, HighlighterTargetArea.EXACT_RANGE,
                ),
            )
        }
        for (g in v.guardRanges()) {
            guardMarkers.add(document.createGuardedBlock(g.first, g.last + 1))
        }
        for (box in v.images) {
            addImageRenderer(v, box)
        }
    }

    private fun fg(span: Span): java.awt.Color? = when {
        span.color != null -> ANSI[span.color]
        span.dim -> JBColor.GRAY
        else -> null
    }

    private fun addImageRenderer(v: ViewModel, box: ImageBox) {
        if (box.row >= v.lineCount()) return
        val startOff = v.lineStart(box.row)
        val endLine = (box.row + box.h - 1).coerceAtMost(v.lineCount() - 1)
        val endOff = v.gridToOffset(endLine, Int.MAX_VALUE)
        val image = imageCache.getOrPut(box.rid) { decode(media(box.rid)) }
        val hl = editor.markupModel.addRangeHighlighter(
            startOff, endOff, HighlighterLayer.SELECTION - 1, null, HighlighterTargetArea.EXACT_RANGE,
        )
        hl.customRenderer = com.intellij.openapi.editor.markup.CustomHighlighterRenderer { ed, _, g ->
            val g2 = g as Graphics2D
            val topLeft = ed.visualPositionToXY(VisualPosition(box.row, box.col))
            val cellW = EditorUtil.getPlainSpaceWidth(ed)
            val rect = Rectangle(topLeft.x, topLeft.y, box.w * cellW, box.h * ed.lineHeight)
            if (image != null) {
                g2.setRenderingHint(
                    RenderingHints.KEY_INTERPOLATION,
                    RenderingHints.VALUE_INTERPOLATION_BILINEAR,
                )
                g2.drawImage(image, rect.x, rect.y, rect.width, rect.height, null)
            } else {
                g2.color = JBColor.GRAY
                g2.drawRect(rect.x, rect.y, rect.width - 1, rect.height - 1)
                val label = box.label.ifEmpty { "image" }
                g2.drawString(label, rect.x + 4, rect.y + ed.lineHeight - 4)
            }
            if (box.bordered && image != null) {
                g2.color = JBColor.border()
                g2.drawRect(rect.x, rect.y, rect.width - 1, rect.height - 1)
            }
        }
        highlighters.add(hl)
    }

    private fun decode(bytes: ByteArray): Image? =
        if (bytes.isEmpty()) null
        else runCatching { ImageIO.read(ByteArrayInputStream(bytes)) }.getOrNull()

    override fun dispose() {
        highlighters.forEach { it.dispose() }
        guardMarkers.forEach { document.removeGuardedBlock(it) }
        EditorFactory.getInstance().releaseEditor(editor)
    }

    companion object {
        /** ANSI palette, light/dark aware. Scheme-derived mapping is a follow-up. */
        private val LINK = JBColor(0x2470B3, 0x589DF6)
        private val ANSI: Map<String, JBColor> = mapOf(
            "Black" to JBColor(0x000000, 0xBBBBBB),
            "Red" to JBColor(0xC91B00, 0xFF6B68),
            "Green" to JBColor(0x00A600, 0xA8C023),
            "Yellow" to JBColor(0xA69C00, 0xD6BF55),
            "Blue" to JBColor(0x2470B3, 0x589DF6),
            "Magenta" to JBColor(0xB3009E, 0xFF99FF),
            "Cyan" to JBColor(0x00A6B3, 0x299999),
            "White" to JBColor(0x808080, 0xFFFFFF),
            "BrightBlack" to JBColor(0x666666, 0x555555),
            "BrightRed" to JBColor(0xFF6B68, 0xFF8785),
            "BrightGreen" to JBColor(0x00D900, 0xA8C023),
            "BrightYellow" to JBColor(0xD6BF55, 0xFFFF00),
            "BrightBlue" to JBColor(0x589DF6, 0x7EAEF1),
            "BrightMagenta" to JBColor(0xFF77FF, 0xFF99FF),
            "BrightCyan" to JBColor(0x00E5E5, 0x6CDADA),
            "BrightWhite" to JBColor(0xA0A0A0, 0xFFFFFF),
        )
    }
}
