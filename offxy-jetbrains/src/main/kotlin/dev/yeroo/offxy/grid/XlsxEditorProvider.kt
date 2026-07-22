package dev.yeroo.offxy.grid

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile

/** The second Offxy editor registration: `.xlsx` workbooks. */
class XlsxEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean =
        file.extension?.equals("xlsx", ignoreCase = true) == true

    override fun createEditor(project: Project, file: VirtualFile): FileEditor =
        XlsxFileEditor(project, file)

    override fun getEditorTypeId(): String = "offxy.xlsx-editor"

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR
}
