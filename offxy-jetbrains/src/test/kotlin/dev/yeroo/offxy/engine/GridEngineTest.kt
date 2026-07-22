package dev.yeroo.offxy.engine

import dev.yeroo.offxy.editor.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GridEngineTest {
    private fun fixture(name: String): ByteArray =
        javaClass.getResourceAsStream("/fixtures/$name")!!.readBytes()

    @Suppress("UNCHECKED_CAST")
    private fun parse(json: String): Map<String, Any?> = Json.parse(json) as Map<String, Any?>

    /** All cell display texts in a view JSON, keyed by "r,c". */
    @Suppress("UNCHECKED_CAST")
    private fun cells(view: Map<String, Any?>): Map<String, String> =
        (view["cells"] as List<Any?>).associate {
            val m = it as Map<String, Any?>
            "${m["r"]},${m["c"]}" to (m["t"] as? String ?: "")
        }

    @Test
    fun opensRealWorkbookAndServesAWindow() {
        GridEngine().use { e ->
            assertTrue(e.open(fixture("sample.xlsx")))
            val v = parse(e.cmd("view\t0\t0\t0\t20\t10"))
            assertTrue("sheets missing", (v["sheets"] as List<*>).isNotEmpty())
            assertTrue("no cells in window", (v["cells"] as List<*>).isNotEmpty())
            assertTrue(v.containsKey("dims"))
        }
    }

    @Test
    fun rejectsGarbage() {
        GridEngine().use { e -> assertFalse(e.open(byteArrayOf(9, 9, 9))) }
    }

    @Test
    fun setRecalculatesDependents() {
        GridEngine().use { e ->
            assertTrue(e.open(GridEngine.newWorkbook()))
            e.cmd("view\t0\t0\t0\t20\t10")
            e.cmd("set\t0\t0\t2")
            e.cmd("set\t1\t0\t3")
            var v = parse(e.cmd("set\t2\t0\t=SUM(A1:A2)"))
            assertEquals("5", cells(v)["2,0"])
            // Change an input: the dependent updates in the same reply.
            v = parse(e.cmd("set\t0\t0\t10"))
            assertEquals("13", cells(v)["2,0"])
            assertTrue(v["dirty"] == true)
        }
    }

    @Test
    fun copyCarriesTsvAndPasteRoundTrips() {
        GridEngine().use { e ->
            assertTrue(e.open(GridEngine.newWorkbook()))
            e.cmd("view\t0\t0\t0\t20\t10")
            e.cmd("set\t0\t0\talpha")
            e.cmd("set\t0\t1\tbeta")
            e.cmd("select\t0\t0\t0\t1")
            val copied = parse(e.cmd("copy"))["copied"] as? String
            assertEquals("alpha\tbeta", copied?.trimEnd())
            val v = parse(e.cmd("paste\t2\t0\t${copied}"))
            assertEquals("alpha", cells(v)["2,0"])
            assertEquals("beta", cells(v)["2,1"])
        }
    }

    @Test
    fun insertRowRewritesReferences() {
        GridEngine().use { e ->
            assertTrue(e.open(GridEngine.newWorkbook()))
            e.cmd("view\t0\t0\t0\t20\t10")
            e.cmd("set\t0\t0\t7")
            e.cmd("set\t2\t0\t=A1*2")
            e.cmd("select\t0\t0\t0\t0")
            val v = parse(e.cmd("insrow\t0\t1"))
            // The value moved down; the formula (now in row 4) still sees it.
            assertEquals("7", cells(v)["1,0"])
            assertEquals("14", cells(v)["3,0"])
        }
    }

    @Test
    fun undoRestoresCellAndStructuralState() {
        GridEngine().use { e ->
            assertTrue(e.open(GridEngine.newWorkbook()))
            e.cmd("view\t0\t0\t0\t20\t10")
            e.cmd("set\t0\t0\tkeep")
            var v = parse(e.cmd("set\t0\t0\treplaced"))
            assertEquals("replaced", cells(v)["0,0"])
            v = parse(e.cmd("undo"))
            assertEquals("keep", cells(v)["0,0"])
            v = parse(e.cmd("redo"))
            assertEquals("replaced", cells(v)["0,0"])
        }
    }

    @Test
    fun saveRoundTripsLosslessly() {
        GridEngine().use { e ->
            assertTrue(e.open(fixture("calc-3d.xlsx")))
            e.cmd("view\t0\t0\t0\t30\t10")
            e.cmd("set\t0\t0\tXYZZY")
            val saved = e.save()
            GridEngine().use { e2 ->
                assertTrue(e2.open(saved))
                val v = parse(e2.cmd("view\t0\t0\t0\t30\t10"))
                assertEquals("XYZZY", cells(v)["0,0"])
            }
        }
    }

    @Test
    fun windowClipsAtSheetEdges() {
        GridEngine().use { e ->
            assertTrue(e.open(GridEngine.newWorkbook()))
            val v = parse(e.cmd("view\t0\t1000\t1000\t50\t50"))
            assertTrue("out-of-range window must serve empty, not crash",
                (v["cells"] as List<*>).isEmpty())
        }
    }

    @Test
    fun ctlServesSheetRead() {
        GridEngine().use { e ->
            assertTrue(e.open(fixture("sample.xlsx")))
            val reply = parse(e.ctl("""{"verb":"sheet.list","args":{}}"""))
            assertTrue("ctl sheet.list failed: $reply", reply["ok"] == true)
        }
    }
}
