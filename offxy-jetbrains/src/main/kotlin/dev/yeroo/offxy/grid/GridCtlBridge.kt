package dev.yeroo.offxy.grid

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import dev.yeroo.offxy.ctl.CtlException
import dev.yeroo.offxy.ctl.CtlServer
import dev.yeroo.offxy.ctl.Discovery
import dev.yeroo.offxy.editor.Json
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicInteger

/**
 * The agent control surface of an open workbook tab: advertises
 * `xlsxy-jetbrains-<basename>-<pid>-<n>` in xlsxy's ctl dir. Host verbs
 * (`wb.path/save/reload/open`) answered in Kotlin; everything else passes
 * through `grid_ctl` (the full xlsxy verb surface). Mutation is detected
 * generically via the engine's `edits` counter — any verb that moved it
 * registers the same engine-stack undo step UI edits use.
 */
object GridCtlBridge {
    private val seq = AtomicInteger(0)

    fun start(project: Project, editor: XlsxFileEditor): CtlServer {
        val instance =
            "xlsxy-jetbrains-${Discovery.sanitize(editor.file.nameWithoutExtension)}-" +
                "${ProcessHandle.current().pid()}-${seq.incrementAndGet()}"
        val server = CtlServer(instance, Discovery.ctlDir("xlsxy")) { verb, args ->
            onEdt(project, editor, verb, args)
        }
        server.start()
        return server
    }

    private fun onEdt(
        project: Project,
        editor: XlsxFileEditor,
        verb: String,
        args: Map<String, Any?>,
    ): Any? {
        val future = CompletableFuture<Any?>()
        ApplicationManager.getApplication().invokeLater {
            try {
                future.complete(handle(project, editor, verb, args))
            } catch (t: Throwable) {
                future.completeExceptionally(t)
            }
        }
        return try {
            future.get(10, TimeUnit.SECONDS)
        } catch (e: TimeoutException) {
            throw CtlException("editor busy: request timed out")
        } catch (e: java.util.concurrent.ExecutionException) {
            throw (e.cause as? CtlException) ?: CtlException(e.cause?.message ?: "error")
        }
    }

    private fun handle(
        project: Project,
        editor: XlsxFileEditor,
        verb: String,
        args: Map<String, Any?>,
    ): Any? {
        if (editor.isDisposed) throw CtlException("editor closed")
        return when (verb) {
            // Internal composition verb, rejected externally on every surface.
            "wb.info" -> throw CtlException("unknown verb 'wb.info'")
            "wb.path" -> pathInfo(editor)
            "wb.save" -> {
                editor.saveNow()
                pathInfo(editor)
            }
            "wb.reload" -> {
                editor.reloadFromDisk(force = true)
                pathInfo(editor)
            }
            "wb.open" -> {
                val path = args["path"] as? String
                    ?: throw CtlException("wb.open needs a 'path' string")
                val file = LocalFileSystem.getInstance().refreshAndFindFileByPath(path)
                    ?: throw CtlException("no such file: $path")
                FileEditorManager.getInstance(project).openFile(file, true)
                linkedMapOf<String, Any?>("path" to path)
            }
            else -> engineVerb(project, editor, verb, args)
        }
    }

    private fun engineVerb(
        project: Project,
        editor: XlsxFileEditor,
        verb: String,
        args: Map<String, Any?>,
    ): Any? {
        val grid = editor.grid ?: throw CtlException("no workbook open")
        val before = grid.view.edits
        val raw = editor.engine().ctl(Json.write(linkedMapOf("verb" to verb, "args" to args)))

        @Suppress("UNCHECKED_CAST")
        val reply = Json.parse(raw) as? Map<String, Any?> ?: throw CtlException("bad engine reply")
        // Refresh the window regardless: reads are cheap, mutations must repaint.
        val v = grid.cmd("view\t${grid.view.active}\t0\t0\t${GridPanel.WINDOW_ROWS}\t${GridPanel.WINDOW_COLS}")
        if (v.edits != before) {
            editor.registerAgentUndo(project, verb)
        }
        if (reply["ok"] == false) {
            throw CtlException(reply["error"] as? String ?: "error")
        }
        return reply["result"] ?: reply.filterKeys { it != "ok" }
    }

    /** `{path, format, modified, sheets, active}` — wb.path composition. */
    private fun pathInfo(editor: XlsxFileEditor): Map<String, Any?> {
        val info = linkedMapOf<String, Any?>(
            "path" to editor.file.path,
            "format" to "xlsx",
            "modified" to editor.isModified,
        )
        editor.grid?.view?.let { v ->
            info["sheets"] = v.sheets.size
            info["active"] = v.active
        }
        return info
    }
}
