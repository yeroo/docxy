package dev.yeroo.offxy.engine

import com.dylibso.chicory.compiler.MachineFactoryCompiler
import com.dylibso.chicory.runtime.ExportFunction
import com.dylibso.chicory.runtime.Instance
import com.dylibso.chicory.wasm.Parser
import com.dylibso.chicory.wasm.WasmModule
import java.util.concurrent.ConcurrentHashMap

/**
 * Shared Chicory marshalling for the offxy wasm engines (docxwasm, gridwasm —
 * both hand-written C-ABI bridges with the same idiom): write input at an
 * `<prefix>_alloc` pointer, call the export, read the `[u32 le len][payload]`
 * result buffer, free it. One Chicory instance per binding (per open
 * document/workbook); NOT thread-safe — callers confine to one thread.
 */
abstract class WasmBinding internal constructor(resource: String, prefix: String) : AutoCloseable {
    protected val instance: Instance =
        Instance.builder(module(resource))
            .withMachineFactory(MachineFactoryCompiler::compile)
            .build()

    private val alloc: ExportFunction = instance.export("${prefix}_alloc")
    private val free: ExportFunction = instance.export("${prefix}_free")

    protected fun writeBytes(data: ByteArray): Long {
        val ptr = alloc.apply(data.size.toLong())[0]
        instance.memory().write(ptr.toInt(), data)
        return ptr
    }

    protected fun readResult(resultPtr: Long): ByteArray {
        val p = resultPtr.toInt()
        val memory = instance.memory()
        val len = memory.readInt(p)
        val payload = memory.readBytes(p + 4, len)
        free.apply(resultPtr, (4 + len).toLong())
        return payload
    }

    /** `fn(handle, ptr, len) -> resultPtr` for byte/string-taking exports. */
    protected fun callWithHandle(fn: ExportFunction, handle: Long, input: ByteArray): ByteArray {
        val ptr = writeBytes(input)
        val result = fn.apply(handle, ptr, input.size.toLong())[0]
        free.apply(ptr, input.size.toLong())
        return readResult(result)
    }

    /** `fn(ptr, len) -> resultPtr` for stateless exports. */
    protected fun callStateless(name: String, input: ByteArray): ByteArray {
        val fn = instance.export(name)
        val ptr = writeBytes(input)
        val result = fn.apply(ptr, input.size.toLong())[0]
        free.apply(ptr, input.size.toLong())
        return readResult(result)
    }

    /** Free an input buffer written with [writeBytes] (open-style calls that
     *  don't run through the helpers). */
    protected fun freeInput(ptr: Long, len: Int) {
        free.apply(ptr, len.toLong())
    }

    companion object {
        private val modules = ConcurrentHashMap<String, WasmModule>()

        private fun module(resource: String): WasmModule =
            modules.computeIfAbsent(resource) {
                val bytes = WasmBinding::class.java.getResourceAsStream(it)
                    ?.readBytes() ?: error("$it missing from plugin resources")
                Parser.parse(bytes)
            }
    }
}
