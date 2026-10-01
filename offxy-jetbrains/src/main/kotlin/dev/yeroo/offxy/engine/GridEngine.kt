package dev.yeroo.offxy.engine

/**
 * The spreadsheet engine: `gridwasm.wasm` on the shared [WasmBinding]
 * marshalling. Commands are gridwasm's tab-delimited dispatch strings
 * (`view`, `select`, `set`, `clear`, `copy`, `cut`, `paste`, `fmt`,
 * `decimals`, `autosum`, `undo`, `redo`, `insrow`/`delrow`/`inscol`/`delcol`,
 * `sheet …`); every command returns the refreshed viewport JSON for the
 * last-requested window (`copy`/`cut` carry the TSV in its `copied` field).
 *
 * One instance per open workbook, EDT-confined, like [ChicoryEngine].
 */
class GridEngine : WasmBinding("/gridwasm.wasm", "grid") {
    private val openFn = instance.export("grid_open")
    private val closeFn = instance.export("grid_close")
    private val cmdFn = instance.export("grid_cmd")
    private val saveFn = instance.export("grid_save")
    private val saveAsFn = instance.export("grid_save_as")
    private val ctlFn = instance.export("grid_ctl")

    private var handle = 0L

    fun open(bytes: ByteArray): Boolean {
        if (handle != 0L) {
            closeFn.apply(handle)
            handle = 0L
        }
        val ptr = writeBytes(bytes)
        handle = openFn.apply(ptr, bytes.size.toLong())[0]
        freeInput(ptr, bytes.size)
        return handle != 0L
    }

    /** Apply one tab-delimited command; returns the refreshed viewport JSON. */
    fun cmd(command: String): String =
        String(callWithHandle(cmdFn, handle, command.toByteArray()))

    /** Serialize back to `.xlsx` bytes, losslessly. */
    fun save(): ByteArray = readResult(saveFn.apply(handle)[0])

    /**
     * Serialize as a file of [fileName]'s type (`.xlsx`, `.xlsm`, `.xltx`,
     * `.xltm`): a template or macro workbook written to `x.xlsx` becomes a
     * real workbook. Any other name keeps the loaded type, as [save] does.
     */
    fun saveAs(fileName: String): ByteArray =
        readResult(saveAsFn.apply(handle, kindCode(fileName))[0])

    /** Service one agent ctl request (`{"verb":…,"args":…}`). */
    fun ctl(requestJson: String): String =
        String(callWithHandle(ctlFn, handle, requestJson.toByteArray()))

    override fun close() {
        if (handle != 0L) {
            closeFn.apply(handle)
            handle = 0L
        }
    }

    companion object {
        /** `grid_save_as`'s code for [fileName]'s extension: 1 = xlsx, 2 = xlsm,
         *  3 = xltx, 4 = xltm, 0 = keep the loaded type. */
        fun kindCode(fileName: String): Long =
            (listOf("xlsx", "xlsm", "xltx", "xltm")
                .indexOf(fileName.substringAfterLast('.', "").lowercase()) + 1).toLong()

        /**
         * Bytes of a fresh empty workbook (`grid_new`), typed for a file
         * named [fileName] ([kindCode]): an empty `.xltm` gets a macro
         * template, not a workbook-typed package Excel refuses under that
         * name. Stateless.
         */
        fun newWorkbook(fileName: String = "Book.xlsx"): ByteArray =
            GridEngine().use { e ->
                val fresh = e.readResult(e.instance.export("grid_new").apply()[0])
                if (kindCode(fileName) <= 1L) {
                    fresh
                } else {
                    check(e.open(fresh)) { "grid_new's workbook did not open" }
                    e.saveAs(fileName)
                }
            }
    }
}
