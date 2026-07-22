package dev.yeroo.offxy.grid

import com.intellij.icons.AllIcons
import com.intellij.openapi.fileTypes.FileType
import com.intellij.openapi.vfs.VirtualFile
import javax.swing.Icon

/** Claims `xlsx` as an IDE-openable (binary) file type — without this the
 *  platform's Native type launches Excel before any editor provider runs
 *  (the docx lesson, applied up front). */
object XlsxFileType : FileType {
    override fun getName(): String = "Offxy Excel Workbook"

    override fun getDescription(): String = "Excel workbook (edited by Offxy)"

    override fun getDefaultExtension(): String = "xlsx"

    override fun getIcon(): Icon = AllIcons.Nodes.DataTables

    override fun isBinary(): Boolean = true

    override fun getCharset(file: VirtualFile, content: ByteArray): String? = null
}
