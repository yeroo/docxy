package dev.yeroo.offxy.grid

import com.intellij.openapi.command.undo.UndoManager
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.GridEngine

class GridSheetsPlatformTest : BasePlatformTestCase() {
    private fun open(): XlsxFileEditor =
        XlsxEditorProvider().createEditor(
            project, BinaryLightVirtualFile("s.xlsx", GridEngine.newWorkbook()),
        ) as XlsxFileEditor

    fun testSheetAddAndSwitch() {
        val editor = open()
        try {
            val grid = editor.grid!!
            val before = grid.view.sheets.size
            editor.runMutating("sheet\tadd\tData")
            assertEquals(before + 1, grid.view.sheets.size)
            assertTrue(grid.view.sheets.contains("Data"))

            grid.cmd("sheet\tswitch\t${grid.view.sheets.indexOf("Data")}")
            assertEquals("Data", grid.view.sheets[grid.view.active])
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testInsertRowShiftsAndUndoes() {
        val editor = open()
        try {
            val grid = editor.grid!!
            grid.model.setValueAt("top", 0, 0)
            grid.model.setValueAt("below", 1, 0)
            editor.runMutating("insrow\t1\t1")
            assertEquals("top", (grid.model.getValueAt(0, 0) as GridCell).text)
            assertNull("inserted row should be blank", grid.model.getValueAt(1, 0))
            assertEquals("below", (grid.model.getValueAt(2, 0) as GridCell).text)

            UndoManager.getInstance(project).undo(editor)
            assertEquals("below", (grid.model.getValueAt(1, 0) as GridCell).text)
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testFmtBoldTogglesTheSelection() {
        val editor = open()
        try {
            val grid = editor.grid!!
            grid.model.setValueAt("emphasized", 0, 0)
            grid.cmd("select\t0\t0")
            editor.runMutating("fmt\tbold\ttoggle")
            assertTrue("cell should render bold", (grid.model.getValueAt(0, 0) as GridCell).bold)
            assertTrue(grid.view.curBold)

            UndoManager.getInstance(project).undo(editor)
            assertFalse((grid.model.getValueAt(0, 0) as GridCell).bold)
        } finally {
            Disposer.dispose(editor)
        }
    }
}
