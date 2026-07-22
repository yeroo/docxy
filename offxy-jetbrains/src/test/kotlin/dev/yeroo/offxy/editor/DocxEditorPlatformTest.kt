package dev.yeroo.offxy.editor

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.editor.ReadOnlyFragmentModificationException
import com.intellij.openapi.editor.ex.DocumentEx
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.ChicoryEngine

/** Platform-level test: the provider claims .docx, the editor shows the
 *  rendered document, and guarded decoration columns reject edits. */
class DocxEditorPlatformTest : BasePlatformTestCase() {
    fun testDocxFileTypeClaimsTheExtension() {
        // Without this mapping the platform treats docx as its Native type and
        // double-click launches Word instead of consulting editor providers.
        val type = com.intellij.openapi.fileTypes.FileTypeManager.getInstance()
            .getFileTypeByFileName("report.docx")
        assertEquals("Offxy Word Document", type.name)
        assertTrue(type.isBinary)
    }

    fun testProviderAcceptsAndRendersDocx() {
        val bytes = ChicoryEngine.fromMarkdown("# Heading\n\nBody text here.\n\n- bullet item\n")
        val file = BinaryLightVirtualFile("t.docx", bytes)
        val provider = DocxEditorProvider()
        assertTrue(provider.accept(project, file))
        assertFalse(provider.accept(project, BinaryLightVirtualFile("t.txt", bytes)))

        val editor = provider.createEditor(project, file) as DocxFileEditor
        try {
            val view = editor.view
            assertNotNull("editor view missing (engine failed to open?)", view)
            val text = view!!.document.text
            assertTrue("render missing heading: $text", text.contains("Heading"))
            assertTrue("render missing body: $text", text.contains("Body text here."))
            assertFalse("should open clean", editor.isModified)

            // The bullet marker columns are guarded: an insert inside them (with
            // guarded-block checking, as editor actions use) must throw.
            val doc = view.document as DocumentEx
            val markerLine = (0 until doc.lineCount).first {
                doc.text.substring(doc.getLineStartOffset(it), doc.getLineEndOffset(it))
                    .contains("bullet item")
            }
            val guarded = doc.getLineStartOffset(markerLine)
            var rejected = false
            WriteAction.run<RuntimeException> {
                doc.startGuardedBlockChecking()
                try {
                    doc.insertString(guarded + 1, "x")
                } catch (e: ReadOnlyFragmentModificationException) {
                    rejected = true
                } finally {
                    doc.stopGuardedBlockChecking()
                }
            }
            assertTrue("insert into the list-marker columns was not rejected", rejected)
        } finally {
            Disposer.dispose(editor)
        }
    }
}
