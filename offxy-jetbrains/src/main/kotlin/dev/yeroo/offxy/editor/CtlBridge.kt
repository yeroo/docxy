package dev.yeroo.offxy.editor

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import dev.yeroo.offxy.ctl.CtlException
import dev.yeroo.offxy.ctl.CtlServer
import dev.yeroo.offxy.ctl.Discovery
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicInteger

/**
 * The agent control surface of one open docx tab: a [CtlServer] advertising
 * `docxy-jetbrains-<basename>-<n>` in docxy's ctl dir, so `docxy --mcp`
 * sessions (Claude Code, Junie) see IDE tabs exactly like terminal panes.
 *
 * Host verbs (`doc.save/reload/open/path`) are answered here; all other
 * `doc.*` verbs go through the engine's `docx_ctl` entry point — which lands
 * with the agent-access plan; until the artifact carries it, they answer the
 * ctl-conformant `not yet implemented`. Requests hop to the EDT (10 s
 * timeout), one in flight per document; mutating verbs land as one platform
 * undo step, exactly like toolbar formatting.
 */
object CtlBridge {
    private val seq = AtomicInteger(0)
    private val MUTATING = setOf(
        "doc.replace-range", "doc.insert", "doc.append",
        "doc.replace-all", "doc.format", "doc.set-style",
    )

    fun start(project: Project, editor: DocxFileEditor): CtlServer {
        // The pid keeps two IDE processes on a same-basename file from
        // minting colliding instance ids (the VS Code tabs' convention).
        val instance =
            "docxy-jetbrains-${Discovery.sanitize(editor.file.nameWithoutExtension)}-" +
                "${ProcessHandle.current().pid()}-${seq.incrementAndGet()}"
        val server = CtlServer(instance) { verb, args -> onEdtWithTimeout(project, editor, verb, args) }
        server.start()
        return server
    }

    private fun onEdtWithTimeout(
        project: Project,
        editor: DocxFileEditor,
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
        editor: DocxFileEditor,
        verb: String,
        args: Map<String, Any?>,
    ): Any? {
        if (editor.isDisposed) throw CtlException("editor closed")
        return when (verb) {
            // Undo is IDE-owned on JetBrains tabs: the engine's internal stack
            // also records replayed user edits, so driving it externally would
            // desync the two stacks. Agents make the inverse edit instead.
            "doc.undo", "doc.redo" ->
                throw CtlException("undo is IDE-owned on JetBrains tabs — make the inverse edit instead")
            // The host-side exclusive-create write is a follow-up.
            "doc.export-pdf" -> throw CtlException("not yet implemented")
            // Internal composition verb, rejected externally on every surface.
            "doc.blocks" -> throw CtlException("unknown verb 'doc.blocks'")
            "doc.path" -> pathInfo(editor)
            "doc.save" -> {
                editor.saveNow()
                pathInfo(editor)
            }
            "doc.reload" -> {
                editor.reloadFromDisk(force = true)
                pathInfo(editor)
            }
            "doc.open" -> {
                val path = args["path"] as? String
                    ?: throw CtlException("doc.open needs a 'path' string")
                val file = LocalFileSystem.getInstance().refreshAndFindFileByPath(path)
                    ?: throw CtlException("no such file: $path")
                FileEditorManager.getInstance(project).openFile(file, true)
                linkedMapOf<String, Any?>("path" to path)
            }
            else -> engineVerb(project, editor, verb, args)
        }
    }

    /** Route a doc verb through `docx_ctl`; mutating verbs get snapshot undo. */
    private fun engineVerb(
        project: Project,
        editor: DocxFileEditor,
        verb: String,
        args: Map<String, Any?>,
    ): Any? {
        val engine = editor.engine()
        val request = Json.write(linkedMapOf("verb" to verb, "args" to args))

        fun call(): Any? {
            val raw = engine.ctl(request) ?: throw CtlException("not yet implemented")

            @Suppress("UNCHECKED_CAST")
            val reply = Json.parse(raw) as? Map<String, Any?>
                ?: throw CtlException("bad engine reply")
            if (reply["ok"] == false) {
                throw CtlException(reply["error"] as? String ?: "error")
            }
            return reply["result"] ?: reply.filterKeys { it != "ok" }
        }

        return if (verb in MUTATING) {
            var result: Any? = null
            Formatting.withSnapshotUndo(project, editor, "Offxy agent: $verb") {
                result = call()
                editor.refreshFrom(engine.render())
            }
            result
        } else {
            call()
        }
    }

    /** `{path, format, modified, blocks}` — control.rs's `path_info` shape.
     *  `blocks` needs the wasm-side `doc.blocks`; omitted until it exists. */
    private fun pathInfo(editor: DocxFileEditor): Map<String, Any?> {
        val info = linkedMapOf<String, Any?>(
            "path" to editor.file.path,
            "format" to "docx",
            "modified" to editor.isModified,
        )
        editor.engine().ctl("""{"verb":"doc.blocks","args":{}}""")?.let { raw ->
            // docx_ctl replies are the result object merged with "ok":true
            // (control.rs's envelope), not nested under a "result" key.
            @Suppress("UNCHECKED_CAST")
            val reply = Json.parse(raw) as? Map<String, Any?>
            if (reply?.get("ok") == true) {
                (reply["total"] as? Long)?.let { info["blocks"] = it }
            }
        }
        return info
    }
}
