package dev.yeroo.offxy.ctl

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.intellij.util.ui.UIUtil
import dev.yeroo.offxy.editor.DocxEditorProvider
import dev.yeroo.offxy.editor.DocxFileEditor
import dev.yeroo.offxy.editor.Json
import dev.yeroo.offxy.engine.ChicoryEngine
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.Socket
import java.nio.file.Files
import java.nio.file.Paths

/** End-to-end: an open editor advertises on the ctl dir; a real TCP client
 *  drives host verbs. Ctl requests hop to the EDT, so the test does socket
 *  I/O on a pooled thread while pumping the EDT. */
class CtlBridgePlatformTest : BasePlatformTestCase() {
    private fun ctlDir() = Paths.get(System.getProperty("offxy.ctl.dir"))

    private fun discoveryFor(name: String): Map<String, Any?> {
        val file = Files.list(ctlDir()).use { stream ->
            stream.filter { it.fileName.toString().let { n -> n.contains(name) && n.endsWith(".json") } }
                .findFirst().orElseThrow { AssertionError("no discovery file for $name") }
        }
        @Suppress("UNCHECKED_CAST")
        return Json.parse(Files.readString(file)) as Map<String, Any?>
    }

    /** Send one request from a pooled thread, pumping the EDT until replied. */
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

    fun testTabAdvertisesAndAnswersHostVerbs() {
        val file = BinaryLightVirtualFile(
            "bridge.docx", ChicoryEngine.fromMarkdown("Bridge test document.\n"),
        )
        val editor = DocxEditorProvider().createEditor(project, file) as DocxFileEditor
        try {
            val disc = discoveryFor("docxy-jetbrains-bridge")
            val port = (disc["port"] as Long).toInt()
            val token = disc["token"] as String
            assertTrue((disc["instance"] as String).startsWith("docxy-jetbrains-bridge-"))

            // doc.path — control.rs path_info shape.
            val path = ctlCall(port, """{"token":"$token","verb":"doc.path","id":1}""")
            assertEquals(true, path["ok"])
            @Suppress("UNCHECKED_CAST")
            val info = path["result"] as Map<String, Any?>
            assertEquals("docx", info["format"])
            assertEquals(false, info["modified"])
            assertTrue((info["path"] as String).contains("bridge.docx"))

            // Wasm-side verbs are conformant-unimplemented until docx_ctl lands
            // (flips to real replies with the agent-access artifact).
            val outline = ctlCall(port, """{"token":"$token","verb":"doc.outline","id":2}""")
            if (outline["ok"] == false) {
                assertEquals("not yet implemented", outline["error"])
            }

            // doc.save writes the engine bytes to the file and clears modified.
            com.intellij.openapi.command.WriteCommandAction.runWriteCommandAction(project) {
                val doc = editor.view!!.document
                doc.insertString(doc.text.indexOf("test"), "SAVED")
            }
            editor.flushEdits()
            assertTrue(editor.isModified)
            val save = ctlCall(port, """{"token":"$token","verb":"doc.save","id":3}""")
            assertEquals(true, save["ok"])
            assertFalse(editor.isModified)
            ChicoryEngine().use { e ->
                assertTrue(e.open(file.contentsToByteArray()))
                assertTrue(
                    "saved bytes must carry the edit",
                    dev.yeroo.offxy.editor.ViewModel(e.render()).text.contains("SAVEDtest"),
                )
            }
        } finally {
            Disposer.dispose(editor)
        }
        assertTrue(
            "dispose must remove the discovery file",
            Files.list(ctlDir()).use { s -> s.noneMatch { it.fileName.toString().contains("bridge") } },
        )
    }
}
