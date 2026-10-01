package dev.yeroo.offxy.grid

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile

/**
 * The second Offxy editor registration: workbooks of every OOXML spreadsheet
 * type (`.xlsx`, `.xlsm`, `.xltx`, `.xltm`). A template is edited in place as
 * a template, and a save writes the file's own type.
 */
class XlsxEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean =
        file.extension?.lowercase() in EXTENSIONS

    override fun createEditor(project: Project, file: VirtualFile): FileEditor =
        XlsxFileEditor(project, file)

    override fun getEditorTypeId(): String = "offxy.xlsx-editor"

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR

    companion object {
        /** The extensions this editor (and [XlsxFileType]) claims. */
        val EXTENSIONS = setOf("xlsx", "xlsm", "xltx", "xltm")
    }
}
