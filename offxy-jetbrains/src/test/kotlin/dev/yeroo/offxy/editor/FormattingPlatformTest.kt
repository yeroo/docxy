package dev.yeroo.offxy.editor

import com.intellij.openapi.command.undo.UndoManager
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.ChicoryEngine

class FormattingPlatformTest : BasePlatformTestCase() {
    private fun open(md: String): DocxFileEditor =
        DocxEditorProvider().createEditor(
            project, BinaryLightVirtualFile("f.docx", ChicoryEngine.fromMarkdown(md)),
        ) as DocxFileEditor

    private fun boldTexts(editor: DocxFileEditor): List<String> {
        val v = ViewModel(editor.engine().render())
        return v.styledRanges().filter { it.second.bold }
            .map { v.text.substring(it.first.first, it.first.last + 1) }
    }

    fun testBoldSelectionAndPlatformUndo() {
        val editor = open("Alpha beta gamma delta.\n")
        try {
            val v = editor.view!!
            val start = v.document.text.indexOf("beta")
            v.editor.selectionModel.setSelection(start, start + 4)

            Formatting.run(project, editor, "bold")
            assertTrue("beta should be bold: ${boldTexts(editor)}",
                boldTexts(editor).any { it.contains("beta") })
            assertTrue("formatting must mark modified", editor.isModified)

            val undo = UndoManager.getInstance(project)
            assertTrue("undo should be available", undo.isUndoAvailable(editor))
            undo.undo(editor)
            com.intellij.util.ui.UIUtil.dispatchAllInvocationEvents()
            assertTrue("bold should be gone after undo: ${boldTexts(editor)}",
                boldTexts(editor).none { it.contains("beta") })

            assertTrue("redo should be available", undo.isRedoAvailable(editor))
            undo.redo(editor)
            com.intellij.util.ui.UIUtil.dispatchAllInvocationEvents()
            assertTrue("bold should be back after redo",
                boldTexts(editor).any { it.contains("beta") })
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testSaveRoundTripsFormattingAndText() {
        val editor = open("Roundtrip body text.\n")
        val file = editor.file
        try {
            val v = editor.view!!
            val at = v.document.text.indexOf("body")
            com.intellij.openapi.command.WriteCommandAction.runWriteCommandAction(project) {
                v.document.insertString(at, "NEW")
            }
            editor.flushEdits()
            editor.saveNow()
            assertFalse("save clears modified", editor.isModified)

            ChicoryEngine().use { e2 ->
                assertTrue(e2.open(file.contentsToByteArray()))
                val text = ViewModel(e2.render()).text
                assertTrue("edit lost in saved bytes: $text", text.contains("NEWbody"))
            }
        } finally {
            Disposer.dispose(editor)
        }
    }
}
