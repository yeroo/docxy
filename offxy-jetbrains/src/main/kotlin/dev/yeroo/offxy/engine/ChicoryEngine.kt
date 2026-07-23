package dev.yeroo.offxy.engine

import com.dylibso.chicory.runtime.ExportFunction

/**
 * [DocxEngine] over the shared `docxwasm.wasm` artifact — a thin client of
 * [WasmBinding]'s marshalling. One wasm instance per engine, so each open
 * document has its own linear memory and its lifetime ends with [close].
 */
class ChicoryEngine : WasmBinding("/docxwasm.wasm", "docx"), DocxEngine {
    private val openFn = instance.export("docx_open")
    private val closeFn = instance.export("docx_close")
    private val renderFn = instance.export("docx_render")
    private val cmdFn = instance.export("docx_cmd")
    private val saveFn = instance.export("docx_save")
    private val mediaFn = instance.export("docx_media")

    /** Present only in artifacts carrying the agent-access work. */
    private val ctlFn: ExportFunction? =
        runCatching { instance.export("docx_ctl") }.getOrNull()

    private var handle = 0L

    override fun open(bytes: ByteArray): Boolean {
        if (handle != 0L) {
            closeFn.apply(handle)
            handle = 0L
        }
        val ptr = writeBytes(bytes)
        handle = openFn.apply(ptr, bytes.size.toLong())[0]
        freeInput(ptr, bytes.size)
        return handle != 0L
    }

    override fun render(): String = String(readResult(renderFn.apply(handle)[0]))

    override fun cmd(command: String): String =
        String(callWithHandle(cmdFn, handle, command.toByteArray()))

    override fun save(): ByteArray = readResult(saveFn.apply(handle)[0])

    override fun media(rid: String): ByteArray =
        callWithHandle(mediaFn, handle, rid.toByteArray())

    override fun ctl(requestJson: String): String? =
        ctlFn?.let { String(callWithHandle(it, handle, requestJson.toByteArray())) }

    override fun close() {
        if (handle != 0L) {
            closeFn.apply(handle)
            handle = 0L
        }
    }

    companion object {
        /** Markdown source → `.docx` bytes (`docx_from_markdown`). Stateless. */
        fun fromMarkdown(md: String): ByteArray =
            ChicoryEngine().use { it.callStateless("docx_from_markdown", md.toByteArray()) }

        /** `.docx` bytes → Markdown source (`docx_to_md`). Stateless. */
        fun toMarkdown(docx: ByteArray): String =
            ChicoryEngine().use { String(it.callStateless("docx_to_md", docx)) }
    }
}
