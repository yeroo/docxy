package dev.yeroo.offxy.grid

import com.intellij.openapi.command.undo.UndoManager
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.GridEngine

class GridEditingPlatformTest : BasePlatformTestCase() {
    private fun open(bytes: ByteArray = GridEngine.newWorkbook()): XlsxFileEditor =
        XlsxEditorProvider().createEditor(project, BinaryLightVirtualFile("e.xlsx", bytes))
            as XlsxFileEditor

    fun testCommitPathUpdatesEngineAndUndoes() {
        val editor = open()
        try {
            val grid = editor.grid!!
            grid.model.setValueAt("hello", 0, 0)
            assertEquals("hello", (grid.model.getValueAt(0, 0) as GridCell).text)
            assertTrue("edit must mark modified", editor.isModified)

            val undo = UndoManager.getInstance(project)
            assertTrue("undo should be available", undo.isUndoAvailable(editor))
            undo.undo(editor)
            assertNull("undo should clear the cell", grid.model.getValueAt(0, 0))
            undo.redo(editor)
            assertEquals("hello", (grid.model.getValueAt(0, 0) as GridCell).text)
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testFormulaRecalcAndFormulaBarSource() {
        val editor = open()
        try {
            val grid = editor.grid!!
            grid.model.setValueAt("3", 0, 0)
            grid.model.setValueAt("4", 1, 0)
            grid.model.setValueAt("=SUM(A1:A2)", 2, 0)
            assertEquals("7", (grid.model.getValueAt(2, 0) as GridCell).text)
            // Select the formula cell: cur.src carries the raw formula.
            val v = grid.cmd("select\t2\t0")
            assertEquals("=SUM(A1:A2)", v.curSrc)
            assertEquals("A3", v.curRef)
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testPasteIsOneUndoStep() {
        val editor = open()
        try {
            val grid = editor.grid!!
            editor.runMutating("paste\t0\t0\ta\tb\nc\td")
            assertEquals("a", (grid.model.getValueAt(0, 0) as GridCell).text)
            assertEquals("d", (grid.model.getValueAt(1, 1) as GridCell).text)
            UndoManager.getInstance(project).undo(editor)
            assertNull(grid.model.getValueAt(0, 0))
            assertNull(grid.model.getValueAt(1, 1))
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testBadFormulaErrorsWithoutMutating() {
        val editor = open()
        try {
            val grid = editor.grid!!
            grid.model.setValueAt("keep", 0, 0)
            val undoBefore = UndoManager.getInstance(project).isUndoAvailable(editor)
            grid.model.setValueAt("=SUM(", 0, 0)
            assertEquals("bad formula must not apply", "keep",
                (grid.model.getValueAt(0, 0) as GridCell).text)
            assertEquals(undoBefore, UndoManager.getInstance(project).isUndoAvailable(editor))
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testSaveRoundTripsEdits() {
        val editor = open()
        val file = editor.file
        try {
            editor.grid!!.model.setValueAt("persisted", 3, 2)
            editor.saveNow()
            assertFalse(editor.isModified)
            GridEngine().use { e ->
                assertTrue(e.open(file.contentsToByteArray()))
                val v = GridViewModel(e.cmd("view\t0\t0\t0\t20\t10"))
                assertEquals("persisted", v.cells[3 to 2]?.text)
            }
        } finally {
            Disposer.dispose(editor)
        }
    }
}
