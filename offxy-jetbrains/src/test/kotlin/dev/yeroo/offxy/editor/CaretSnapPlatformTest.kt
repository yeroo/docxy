package dev.yeroo.offxy.editor

import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.ChicoryEngine

/** The caret never rests on decorations; selections over them are untouched. */
class CaretSnapPlatformTest : BasePlatformTestCase() {
    private fun open(md: String): DocxFileEditor =
        DocxEditorProvider().createEditor(
            project, BinaryLightVirtualFile("c.docx", ChicoryEngine.fromMarkdown(md)),
        ) as DocxFileEditor

    fun testCaretSnapsOutOfListMarkerColumns() {
        val editor = open("- item one\n- item two\n")
        try {
            val view = editor.view!!
            val v = view.currentView()!!
            val markerLine = (0 until v.lineCount())
                .first { line -> v.segs[line].isNotEmpty() && v.segs[line].first().first > 0 }
            val segStart = v.segs[markerLine].first().first
            // Land the caret inside the marker columns (col 0 of the item).
            view.editor.caretModel.moveToOffset(v.lineStart(markerLine))
            val col = view.editor.caretModel.logicalPosition.column
            assertTrue(
                "caret should have snapped past the marker (col=$col, segStart=$segStart)",
                col >= segStart,
            )
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testCaretHopsOffDecorationOnlyLines() {
        val editor = open("# Heading\n\nBody text.\n")
        try {
            val view = editor.view!!
            val v = view.currentView()!!
            val bareLine = (0 until v.lineCount()).firstOrNull { v.segs[it].isEmpty() && v.lines[it].isNotEmpty() }
            if (bareLine == null) return // render has no decoration-only line — nothing to test
            view.editor.caretModel.moveToOffset(v.lineStart(bareLine))
            val landed = view.editor.caretModel.logicalPosition.line
            assertTrue(
                "caret should not rest on decoration-only line $bareLine (landed $landed)",
                v.segs.getOrNull(landed)?.isNotEmpty() == true || v.lines[landed].isEmpty(),
            )
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testSelectionAcrossDecorationsIsNotDisturbed() {
        val editor = open("- alpha\n- beta\n")
        try {
            val view = editor.view!!
            val doc = view.document
            // Select everything — markers included — as a user copying a block.
            view.editor.selectionModel.setSelection(0, doc.textLength)
            view.editor.caretModel.moveToOffset(0) // caret inside marker cols, selection active
            assertTrue("selection must survive caret snap logic", view.editor.selectionModel.hasSelection())
            assertEquals(0, view.editor.selectionModel.selectionStart)
            assertEquals(doc.textLength, view.editor.selectionModel.selectionEnd)
        } finally {
            Disposer.dispose(editor)
        }
    }
}
