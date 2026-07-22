package dev.yeroo.offxy.grid

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.intellij.util.ui.UIUtil
import dev.yeroo.offxy.editor.Json
import dev.yeroo.offxy.engine.GridEngine
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.Socket
import java.nio.file.Files
import java.nio.file.Paths

class GridCtlBridgePlatformTest : BasePlatformTestCase() {
    private fun ctlDir() = Paths.get(System.getProperty("offxy.ctl.dir")).resolve("xlsxy")

    private fun discoveryFor(name: String): Map<String, Any?> {
        val file = Files.list(ctlDir()).use { stream ->
            stream.filter { it.fileName.toString().let { n -> n.contains(name) && n.endsWith(".json") } }
                .findFirst().orElseThrow { AssertionError("no discovery file for $name") }
        }
        @Suppress("UNCHECKED_CAST")
        return Json.parse(Files.readString(file)) as Map<String, Any?>
    }

    private fun ctlCall(port: Int, line: String): Map<String, Any?> {
        val task = ApplicationManager.getApplication().executeOnPooledThread<String> {
            Socket("127.0.0.1", port).use { s ->
                s.getOutputStream().write((line + "\n").toByteArray())
                s.getOutputStream().flush()
                BufferedReader(InputStreamReader(s.getInputStream())).readLine()
            }
        }
        val deadline = System.currentTimeMillis() + 15_000
        while (!task.isDone && System.currentTimeMillis() < deadline) {
            UIUtil.dispatchAllInvocationEvents()
            Thread.sleep(5)
        }
        assertTrue("ctl call timed out", task.isDone)
        @Suppress("UNCHECKED_CAST")
        return Json.parse(task.get()) as Map<String, Any?>
    }

    fun testWorkbookAdvertisesAndServesVerbs() {
        val editor = XlsxEditorProvider().createEditor(
            project, BinaryLightVirtualFile("ledger.xlsx", GridEngine.newWorkbook()),
        ) as XlsxFileEditor
        try {
            val disc = discoveryFor("xlsxy-jetbrains-ledger")
            val port = (disc["port"] as Long).toInt()
            val token = disc["token"] as String

            val path = ctlCall(port, """{"token":"$token","verb":"wb.path","id":1}""")
            assertEquals(true, path["ok"])
            @Suppress("UNCHECKED_CAST")
            val info = path["result"] as Map<String, Any?>
            assertEquals("xlsx", info["format"])
            assertEquals(false, info["modified"])

            // Engine verb passthrough: an agent cell.set repaints the grid,
            // marks modified, and is one platform undo step.
            val set = ctlCall(port, """{"token":"$token","verb":"cell.set","args":{"ref":"B2","text":"77"},"id":2}""")
            assertEquals(true, set["ok"])
            assertEquals("77", (editor.grid!!.model.getValueAt(1, 1) as GridCell).text)
            assertTrue(editor.isModified)
            val undo = com.intellij.openapi.command.undo.UndoManager.getInstance(project)
            assertTrue(undo.isUndoAvailable(editor))
            undo.undo(editor)
            assertNull("agent edit should be one Ctrl+Z away", editor.grid!!.model.getValueAt(1, 1))

            // Read verb.
            val sheets = ctlCall(port, """{"token":"$token","verb":"sheet.list","id":3}""")
            assertEquals(true, sheets["ok"])

            // Internal composition verb rejected.
            val infoVerb = ctlCall(port, """{"token":"$token","verb":"wb.info","id":4}""")
            assertEquals("unknown verb 'wb.info'", infoVerb["error"])
        } finally {
            Disposer.dispose(editor)
        }
        assertTrue(
            "dispose must remove the discovery file",
            Files.list(ctlDir()).use { s -> s.noneMatch { it.fileName.toString().contains("ledger") } },
        )
    }
}
