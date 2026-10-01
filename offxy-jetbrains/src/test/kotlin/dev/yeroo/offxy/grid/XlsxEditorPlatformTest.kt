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

    /** #789: every OOXML workbook type is claimed, in any case; others are not. */
    fun testEveryWorkbookKindIsClaimed() {
        val types = com.intellij.openapi.fileTypes.FileTypeManager.getInstance()
        val provider = XlsxEditorProvider()
        for (name in listOf("book.xlsx", "book.xlsm", "book.xltx", "book.xltm", "BOOK.XLTM")) {
            assertEquals(name, "Offxy Excel Workbook", types.getFileTypeByFileName(name).name)
            assertTrue(name, provider.accept(project, BinaryLightVirtualFile(name, knownWorkbook())))
        }
        for (name in listOf("book.xls", "book.csv", "book.docx", "book")) {
            assertFalse(name, provider.accept(project, BinaryLightVirtualFile(name, ByteArray(0))))
        }
    }

    /** #789: "Create new workbook" over an empty file writes that file's own
     *  type, so an empty `.xltm` becomes a macro template Excel opens. */
    fun testCreatingInAnEmptyFileWritesItsOwnType() {
        val cases = listOf(
            "t.xlsx" to "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            "t.xlsm" to "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
            "t.xltx" to "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
            "t.xltm" to "application/vnd.ms-excel.template.macroEnabled.main+xml",
        )
        for ((name, contentType) in cases) {
            val file = BinaryLightVirtualFile(name, ByteArray(0))
            val editor = XlsxEditorProvider().createEditor(project, file) as XlsxFileEditor
            try {
                editor.createNewWorkbook()
                val types = zipEntry(file.contentsToByteArray(), "[Content_Types].xml")
                assertTrue("$name: $types", types.contains("ContentType=\"$contentType\""))
                assertNotNull("$name did not open", editor.grid)
            } finally {
                Disposer.dispose(editor)
            }
        }
    }

    private fun zipEntry(zip: ByteArray, name: String): String {
        java.util.zip.ZipInputStream(zip.inputStream()).use { z ->
            while (true) {
                val entry = z.nextEntry ?: break
                if (entry.name == name) return String(z.readBytes())
            }
        }
        fail("no $name in the package")
        error("unreachable")
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
