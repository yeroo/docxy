package dev.yeroo.offxy.engine

import com.dylibso.chicory.compiler.MachineFactoryCompiler
import com.dylibso.chicory.runtime.ExportFunction
import com.dylibso.chicory.runtime.Instance
import com.dylibso.chicory.wasm.Parser
import com.dylibso.chicory.wasm.WasmModule

/**
 * [DocxEngine] over the shared `docxwasm.wasm` artifact, executed by Chicory
 * (pure-JVM wasm runtime — no native code, no per-platform builds). One wasm
 * instance per engine, so each open document has its own linear memory and its
 * lifetime ends with [close].
 *
 * The marshalling is a port of `offxy-vscode/media/webview.js`: inputs are
 * written at `docx_alloc` pointers; results are length-prefixed buffers
 * (`[u32 le len][payload]`) freed with `docx_free(ptr, 4 + len)`.
 */
class ChicoryEngine : DocxEngine {
    private val instance: Instance =
        Instance.builder(MODULE)
            .withMachineFactory(MachineFactoryCompiler::compile)
            .build()

    private val alloc = instance.export("docx_alloc")
    private val free = instance.export("docx_free")
    private val openFn = instance.export("docx_open")
    private val closeFn = instance.export("docx_close")
    private val renderFn = instance.export("docx_render")
    private val cmdFn = instance.export("docx_cmd")
    private val saveFn = instance.export("docx_save")
    private val mediaFn = instance.export("docx_media")

    /** Present only once the agent-access work lands in the artifact. */
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
        free.apply(ptr, bytes.size.toLong())
        return handle != 0L
    }

    override fun render(): String = String(readResult(renderFn.apply(handle)[0]))

    override fun cmd(command: String): String =
        String(callWithHandle(cmdFn, command.toByteArray()))

    override fun save(): ByteArray = readResult(saveFn.apply(handle)[0])

    override fun media(rid: String): ByteArray = callWithHandle(mediaFn, rid.toByteArray())

    override fun ctl(requestJson: String): String? =
        ctlFn?.let { String(callWithHandle(it, requestJson.toByteArray())) }

    override fun close() {
        if (handle != 0L) {
            closeFn.apply(handle)
            handle = 0L
        }
    }

    // ---- marshalling --------------------------------------------------------

    private fun writeBytes(data: ByteArray): Long {
        val ptr = alloc.apply(data.size.toLong())[0]
        instance.memory().write(ptr.toInt(), data)
        return ptr
    }

    private fun readResult(resultPtr: Long): ByteArray {
        val p = resultPtr.toInt()
        val memory = instance.memory()
        val len = memory.readInt(p)
        val payload = memory.readBytes(p + 4, len)
        free.apply(resultPtr, (4 + len).toLong())
        return payload
    }

    /** `fn(handle, ptr, len) -> resultPtr` for byte/string-taking exports. */
    private fun callWithHandle(fn: ExportFunction, input: ByteArray): ByteArray {
        val ptr = writeBytes(input)
        val result = fn.apply(handle, ptr, input.size.toLong())[0]
        free.apply(ptr, input.size.toLong())
        return readResult(result)
    }

    /** `fn(ptr, len) -> resultPtr` for the stateless conversion exports. */
    private fun statelessCall(name: String, input: ByteArray): ByteArray {
        val fn = instance.export(name)
        val ptr = writeBytes(input)
        val result = fn.apply(ptr, input.size.toLong())[0]
        free.apply(ptr, input.size.toLong())
        return readResult(result)
    }

    companion object {
        /** Parsed once; each engine builds (and JIT-compiles) its own instance. */
        private val MODULE: WasmModule by lazy {
            val bytes = ChicoryEngine::class.java.getResourceAsStream("/docxwasm.wasm")
                ?.readBytes() ?: error("docxwasm.wasm missing from plugin resources")
            Parser.parse(bytes)
        }

        /** Markdown source → `.docx` bytes (`docx_from_markdown`). Stateless. */
        fun fromMarkdown(md: String): ByteArray =
            ChicoryEngine().use { it.statelessCall("docx_from_markdown", md.toByteArray()) }

        /** `.docx` bytes → Markdown source (`docx_to_md`). Stateless. */
        fun toMarkdown(docx: ByteArray): String =
            ChicoryEngine().use { String(it.statelessCall("docx_to_md", docx)) }
    }
}
