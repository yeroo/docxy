package dev.yeroo.offxy.editor

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile

/** Claims `*.docx` files for the Offxy editor. The registration seam for the
 *  future xlsx grid editor is a sibling provider, as in the VS Code design. */
class DocxEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean =
        file.extension?.equals("docx", ignoreCase = true) == true

    override fun createEditor(project: Project, file: VirtualFile): FileEditor =
        DocxFileEditor(project, file)

    override fun getEditorTypeId(): String = "offxy.docx-editor"

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR
}
