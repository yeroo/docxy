package dev.yeroo.offxy.editor

import dev.yeroo.offxy.engine.ChicoryEngine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ViewModelTest {
    private fun render(md: String, width: Int = 60): ViewModel =
        ChicoryEngine().use { e ->
            check(e.open(ChicoryEngine.fromMarkdown(md)))
            e.cmd("width\t$width")
            ViewModel(e.render())
        }

    @Test
    fun textJoinsRenderedLines() {
        val v = render("# Title\n\nHello world.\n")
        assertTrue("title missing: ${v.text}", v.text.contains("Title"))
        assertTrue("body missing: ${v.text}", v.text.contains("Hello world."))
        assertEquals(v.lineCount(), v.text.split('\n').size)
    }

    @Test
    fun styledRangesCarryBold() {
        val v = render("plain **bolded** plain\n")
        val bold = v.styledRanges().filter { it.second.bold }
        assertTrue("no bold ranges", bold.isNotEmpty())
        val (range, _) = bold.first()
        assertEquals("bolded", v.text.substring(range.first, range.last + 1))
    }

    @Test
    fun offsetsRoundTripThroughGrid() {
        val v = render("First paragraph.\n\nSecond one here.\n")
        for (offset in intArrayOf(0, 3, v.text.length / 2, v.text.length)) {
            val (line, col) = v.offsetToGrid(offset)
            val back = v.gridToOffset(line, col)
            val (line2, col2) = v.offsetToGrid(back)
            assertEquals(line, line2)
            assertEquals(col, col2)
        }
    }

    @Test
    fun listMarkerColumnsAreGuarded() {
        val v = render("- item one\n- item two\n")
        val markerLine = v.lines.indexOfFirst { line -> line.any { it.text.contains("item one") } }
        assertTrue("list line not found: ${v.text}", markerLine >= 0)
        val lineStart = v.lineStart(markerLine)
        val guards = v.guardRanges().filter { it.first >= lineStart && it.first < lineStart + 8 }
        assertTrue("marker columns should be guarded (segs=${v.segs.getOrNull(markerLine)})", guards.isNotEmpty())
        val segStart = v.segs[markerLine].minOf { it.first }
        assertTrue("seg should start past the marker", segStart > 0)
    }

    @Test
    fun guardsComplementSegs() {
        val v = render("Some plain text.\n")
        // A plain paragraph line is fully editable: no guard inside its seg span.
        val line = v.lines.indexOfFirst { l -> l.any { it.text.contains("Some plain") } }
        val seg = v.segs[line].first()
        val lineStart = v.lineStart(line)
        for (g in v.guardRanges()) {
            assertTrue(
                "guard $g overlaps seg $seg",
                g.last < lineStart + seg.first || g.first > lineStart + seg.last,
            )
        }
    }
}
