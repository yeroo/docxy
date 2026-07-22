package dev.yeroo.offxy.editor

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.actions.MarkdownConvert
import dev.yeroo.offxy.engine.ChicoryEngine

class Task6PlatformTest : BasePlatformTestCase() {
    private fun open(file: com.intellij.openapi.vfs.VirtualFile): DocxFileEditor =
        DocxEditorProvider().createEditor(project, file) as DocxFileEditor

    fun testEmptyFileCreateFlow() {
        val file = BinaryLightVirtualFile("empty.docx", ByteArray(0))
        val editor = open(file)
        try {
            assertNull("empty file must not open a view", editor.view)
            editor.createNewDocument()
            assertNotNull("create flow should build the view", editor.view)
            assertTrue("file should hold a real document now", file.contentsToByteArray().isNotEmpty())
            ChicoryEngine().use { e ->
                assertTrue("minted bytes must reopen", e.open(file.contentsToByteArray()))
            }
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testReloadFollowsDiskWhenUnmodified() {
        val file = BinaryLightVirtualFile("r.docx", ChicoryEngine.fromMarkdown("Original text.\n"))
        val editor = open(file)
        try {
            assertTrue(editor.view!!.document.text.contains("Original text."))
            val newBytes = ChicoryEngine.fromMarkdown("Replaced content.\n")
            WriteAction.run<RuntimeException> { file.setBinaryContent(newBytes) }
            editor.reloadFromDisk()
            val text = editor.view!!.document.text
            assertTrue("reload should show new content: $text", text.contains("Replaced content."))
            assertFalse("reload keeps the tab clean", editor.isModified)
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testMarkdownConvertAndExportRoundTrip() {
        val md = myFixture.tempDirFixture.createFile(
            "note.md", "# Converted\n\nBody with **bold** text.\n\n- item\n",
        )
        val docx = MarkdownConvert.mdToDocx(md)
        assertEquals("note.docx", docx.name)
        val editor = open(docx)
        try {
            val text = editor.view!!.document.text
            assertTrue("converted doc should render heading: $text", text.contains("Converted"))

            val back = MarkdownConvert.docxToMd(editor)
            val backText = String(back.contentsToByteArray(), Charsets.UTF_8)
            assertTrue("heading lost: $backText", backText.contains("# Converted"))
            assertTrue("bold lost: $backText", backText.contains("**bold**"))
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testReplaceAllViaFormattingIsOneUndoStep() {
        val editor = open(BinaryLightVirtualFile("rep.docx", ChicoryEngine.fromMarkdown("red fish red fish\n")))
        try {
            Formatting.run(project, editor, "replace\tred\tblue")
            val text = editor.view!!.document.text
            assertTrue("replace failed: $text", text.contains("blue fish blue fish"))

            val undo = com.intellij.openapi.command.undo.UndoManager.getInstance(project)
            assertTrue(undo.isUndoAvailable(editor))
            undo.undo(editor)
            com.intellij.util.ui.UIUtil.dispatchAllInvocationEvents()
            assertTrue(
                "undo should restore both reds: ${editor.view!!.document.text}",
                editor.view!!.document.text.contains("red fish red fish"),
            )
        } finally {
            Disposer.dispose(editor)
        }
    }
}
