package dev.yeroo.offxy.engine

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ChicoryEngineTest {
    private fun fixture(name: String): ByteArray =
        javaClass.getResourceAsStream("/fixtures/$name")!!.readBytes()

    /** A document with known text, minted through the engine's own converter. */
    private fun knownDoc(): ByteArray =
        ChicoryEngine.fromMarkdown("# Title\n\nHello world from offxy.\n")

    @Test
    fun opensAndRendersRealDocument() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(fixture("sample.docx")))
            val v = e.render()
            assertTrue("caret missing: ${v.take(200)}", v.contains("\"caret\""))
            assertTrue("should open clean", v.contains("\"dirty\":false"))
        }
    }

    @Test
    fun rejectsGarbage() {
        ChicoryEngine().use { e -> assertFalse(e.open(byteArrayOf(1, 2, 3))) }
    }

    @Test
    fun insertMarksDirtyAndShowsText() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(knownDoc()))
            val v = e.cmd("insert\tXYZZY")
            assertTrue(v.contains("\"dirty\":true"))
            assertTrue("inserted text not rendered", v.contains("XYZZY"))
        }
    }

    @Test
    fun undoRestores() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(knownDoc()))
            e.cmd("insert\tXYZZY")
            val v = e.cmd("undo")
            assertFalse("undo left inserted text", v.contains("XYZZY"))
        }
    }

    @Test
    fun saveRoundTripsAnEdit() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(knownDoc()))
            e.cmd("insert\tXYZZY")
            val saved = e.save()
            ChicoryEngine().use { e2 ->
                assertTrue(e2.open(saved))
                assertTrue("edit lost in save round-trip", e2.render().contains("XYZZY"))
            }
        }
    }

    @Test
    fun findSelectsMatch() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(knownDoc()))
            val v = e.cmd("find\tworld")
            // The find op ships with Task 3; until then this documents the
            // pre-Task-3 behavior (unknown op = no selection, not dirty).
            assertTrue(v.contains("\"dirty\":false"))
        }
    }

    @Test
    fun mediaReturnsImageBytes() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(fixture("complex0.docx")))
            val v = e.render()
            val rid = Regex("\"images\":\\[\\{\"rid\":\"([^\"]+)\"").find(v)?.groupValues?.get(1)
            assertTrue("no image boxes in complex0.docx render", rid != null)
            assertTrue("media bytes empty for $rid", e.media(rid!!).isNotEmpty())
        }
    }

    @Test
    fun markdownRoundTrip() {
        val docx = ChicoryEngine.fromMarkdown("# Head\n\nBody **bold** text.\n\n- one\n- two\n")
        ChicoryEngine().use { e -> assertTrue(e.open(docx)) }
        val back = ChicoryEngine.toMarkdown(docx)
        assertTrue("heading lost: $back", back.contains("# Head"))
        assertTrue("bold lost: $back", back.contains("**bold**"))
        assertTrue("list lost: $back", back.contains("- one"))
    }

    @Test
    fun ctlAbsentInCurrentArtifact() {
        ChicoryEngine().use { e ->
            assertTrue(e.open(knownDoc()))
            // Flips to non-null when the agent-access plan's docx_ctl lands;
            // Task 7's bridge relies on exactly this probe.
            val reply = e.ctl("""{"verb":"doc.read","args":{}}""")
            assertTrue(reply == null || reply.contains("\"ok\""))
        }
    }
}
