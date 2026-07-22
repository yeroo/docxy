package dev.yeroo.offxy.grid

import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.GridEngine

class XlsxEditorPlatformTest : BasePlatformTestCase() {
    /** A workbook with known content, minted through the engine itself. */
    private fun knownWorkbook(): ByteArray =
        GridEngine().let { e ->
            check(e.open(GridEngine.newWorkbook()))
            e.cmd("view\t0\t0\t0\t20\t10")
            e.cmd("set\t0\t0\talpha")
            e.cmd("set\t1\t1\t42")
            e.cmd("set\t2\t0\t=B2*2")
            val bytes = e.save()
            e.close()
            bytes
        }

    fun testXlsxFileTypeClaimsTheExtension() {
        val type = com.intellij.openapi.fileTypes.FileTypeManager.getInstance()
            .getFileTypeByFileName("book.xlsx")
        assertEquals("Offxy Excel Workbook", type.name)
        assertTrue(type.isBinary)
    }

    fun testProviderRendersWorkbookValues() {
        val file = BinaryLightVirtualFile("t.xlsx", knownWorkbook())
        val provider = XlsxEditorProvider()
        assertTrue(provider.accept(project, file))
        val editor = provider.createEditor(project, file) as XlsxFileEditor
        try {
            val grid = editor.grid
            assertNotNull("grid missing (engine failed to open?)", grid)
            val model = grid!!.model
            assertEquals("alpha", (model.getValueAt(0, 0) as GridCell).text)
            assertEquals("42", (model.getValueAt(1, 1) as GridCell).text)
            assertEquals("84", (model.getValueAt(2, 0) as GridCell).text)
            assertEquals("A", model.getColumnName(0))
            assertEquals("AA", model.getColumnName(26))
            assertFalse("should open clean", editor.isModified)
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testWindowRefreshServesScrolledCells() {
        // Value far outside the initial window: request the window over it.
        val bytes = GridEngine().let { e ->
            check(e.open(GridEngine.newWorkbook()))
            e.cmd("view\t0\t0\t0\t20\t10")
            e.cmd("set\t150\t2\tdeep")
            val b = e.save(); e.close(); b
        }
        val editor = XlsxEditorProvider().createEditor(project, BinaryLightVirtualFile("d.xlsx", bytes))
            as XlsxFileEditor
        try {
            val grid = editor.grid!!
            assertNull("cell should be outside the initial window", grid.model.getValueAt(150, 2))
            grid.requestWindow(140, 0)
            assertEquals("deep", (grid.model.getValueAt(150, 2) as GridCell).text)
        } finally {
            Disposer.dispose(editor)
        }
    }
}
