package dev.yeroo.offxy.editor

import com.intellij.icons.AllIcons
import com.intellij.openapi.fileTypes.FileType
import com.intellij.openapi.vfs.VirtualFile
import javax.swing.Icon

/**
 * Claims the `docx` extension as an IDE-openable (binary) file type. Without
 * this, the platform maps `docx` to its Native file type ("open in associated
 * application") and launches Word/LibreOffice before any FileEditorProvider
 * is consulted.
 */
object DocxFileType : FileType {
    override fun getName(): String = "Offxy Word Document"

    override fun getDescription(): String = "Word document (edited by Offxy)"

    override fun getDefaultExtension(): String = "docx"

    override fun getIcon(): Icon = AllIcons.FileTypes.Text

    override fun isBinary(): Boolean = true

    override fun getCharset(file: VirtualFile, content: ByteArray): String? = null
}
