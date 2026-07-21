package dev.yeroo.offxy.engine

/**
 * The document engine seam: everything the editor UI and the agent ctl bridge
 * need from docxwasm, hidden behind an interface so the Chicory-based
 * implementation can be swapped for a native (Panama FFI) one without touching
 * callers.
 *
 * Implementations are NOT thread-safe; callers confine all calls for one
 * engine to a single thread (the editor uses the EDT).
 */
interface DocxEngine : AutoCloseable {
    /** Load a `.docx` from its raw bytes, replacing any open document. Returns
     *  false if the container can't be parsed. */
    fun open(bytes: ByteArray): Boolean

    /** Render the current document to the view JSON (styled lines, caret,
     *  selection flag, dirty flag, image boxes). */
    fun render(): String

    /** Apply one tab-delimited command (see docxwasm's `Session::dispatch`)
     *  and return the refreshed view JSON. */
    fun cmd(command: String): String

    /** Serialize the document back to `.docx` bytes, losslessly. */
    fun save(): ByteArray

    /** Raw bytes of the embedded media for a relationship id from the view's
     *  `images` array (empty if unknown). */
    fun media(rid: String): ByteArray

    /** Service one agent ctl request (`{"verb":…,"args":…}` JSON), or null if
     *  this engine artifact predates the `docx_ctl` export. */
    fun ctl(requestJson: String): String?
}
