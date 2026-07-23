package dev.yeroo.offxy.actions

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import dev.yeroo.offxy.editor.DocxFileEditor
import dev.yeroo.offxy.engine.ChicoryEngine

/** Testable cores of the markdown ⇄ docx actions (extension.ts parity). */
object MarkdownConvert {
    /** `report.md` → sibling `report.docx` via the engine's converter. */
    fun mdToDocx(mdFile: VirtualFile): VirtualFile {
        val md = String(mdFile.contentsToByteArray(), Charsets.UTF_8)
        val bytes = ChicoryEngine.fromMarkdown(md)
        return writeSibling(mdFile, mdFile.nameWithoutExtension + ".docx", bytes)
    }

    /** Active editor's current content → sibling `.md`. */
    fun docxToMd(editor: DocxFileEditor): VirtualFile {
        val md = ChicoryEngine.toMarkdown(editor.engine().save())
        val file = editor.file
        return writeSibling(file, file.nameWithoutExtension + ".md", md.toByteArray(Charsets.UTF_8))
    }

    private fun writeSibling(source: VirtualFile, name: String, bytes: ByteArray): VirtualFile =
        WriteAction.compute<VirtualFile, RuntimeException> {
            val dir = source.parent
            val target = dir.findChild(name) ?: dir.createChildData(source, name)
            target.setBinaryContent(bytes)
            target
        }
}

internal fun activeOffxyEditor(project: Project?): DocxFileEditor? =
    project?.let { FileEditorManager.getInstance(it).selectedEditor as? DocxFileEditor }

/** Project-view action: convert a selected `.md` to a sibling `.docx`. */
class ConvertMarkdownAction : AnAction("Convert Markdown to Word (.docx)") {
    override fun update(e: AnActionEvent) {
        val file = e.getData(CommonDataKeys.VIRTUAL_FILE)
        e.presentation.isEnabledAndVisible =
            file?.extension?.equals("md", ignoreCase = true) == true
    }

    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val md = e.getData(CommonDataKeys.VIRTUAL_FILE) ?: return
        val docx = MarkdownConvert.mdToDocx(md)
        FileEditorManager.getInstance(project).openFile(docx, true)
    }
}

/** Export the focused Offxy document to a sibling `.md` and open it. */
class ExportMarkdownAction : AnAction("Offxy: Export to Markdown") {
    override fun update(e: AnActionEvent) {
        e.presentation.isEnabled = activeOffxyEditor(e.project)?.view != null
    }

    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val editor = activeOffxyEditor(project) ?: return
        val md = MarkdownConvert.docxToMd(editor)
        FileEditorManager.getInstance(project).openFile(md, true)
    }
}
