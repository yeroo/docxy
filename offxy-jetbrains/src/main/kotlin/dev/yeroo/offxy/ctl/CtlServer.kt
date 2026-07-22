package dev.yeroo.offxy.ctl

import com.intellij.openapi.Disposable
import dev.yeroo.offxy.editor.Json
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.nio.file.Files
import java.nio.file.Path
import java.security.SecureRandom

/** An error a verb handler raises to produce an `ok:false` reply. */
class CtlException(message: String) : Exception(message)

/**
 * Ctlcore's wire protocol in Kotlin: loopback-only TCP, one JSON object per
 * line, token checked on every request, one reply line per request, `id`
 * echoed when present. Error strings match ctlcore's (`invalid json: …`,
 * `unauthorized: bad or missing token`, `missing verb`). A discovery file
 * advertises the instance and is re-written on a timer if an external
 * stale-sweep removes it while we're alive.
 *
 * The handler runs on a connection thread, serialized across the server (one
 * in-flight request), and may throw [CtlException] for a clean error reply.
 */
class CtlServer(
    val instance: String,
    private val dir: Path = Discovery.ctlDir(),
    private val refreshMillis: Long = 30_000,
    private val handler: (verb: String, args: Map<String, Any?>) -> Any?,
) : Disposable {
    private val listener = ServerSocket(0, 16, InetAddress.getLoopbackAddress())
    val port: Int = listener.localPort
    val token: String = mintToken()

    private val handlerLock = Any()

    @Volatile
    private var closed = false

    fun start() {
        Discovery.write(dir, instance, port, token)
        thread("ctl-accept-$instance") {
            while (!closed) {
                val socket = runCatching { listener.accept() }.getOrNull() ?: break
                thread("ctl-conn-$instance") { serveConnection(socket) }
            }
        }
        thread("ctl-refresh-$instance") {
            while (!closed) {
                Thread.sleep(refreshMillis)
                if (closed) break
                if (!Files.exists(Discovery.path(dir, instance))) {
                    runCatching { Discovery.write(dir, instance, port, token) }
                }
            }
        }
    }

    private fun serveConnection(socket: Socket) {
        socket.use { s ->
            val reader = BufferedReader(InputStreamReader(s.getInputStream(), Charsets.UTF_8))
            val out = s.getOutputStream()
            while (!closed) {
                val line = runCatching { reader.readLine() }.getOrNull() ?: return
                if (line.isBlank()) continue
                val reply = dispatchLine(line.trim())
                runCatching {
                    out.write((reply + "\n").toByteArray(Charsets.UTF_8))
                    out.flush()
                }.onFailure { return }
            }
        }
    }

    private fun dispatchLine(line: String): String {
        val msg = try {
            @Suppress("UNCHECKED_CAST")
            Json.parse(line) as? Map<String, Any?> ?: return errLine(null, "invalid json: not an object")
        } catch (e: Exception) {
            return errLine(null, "invalid json: ${e.message ?: "parse error"}")
        }
        val id = msg["id"]
        if (msg["token"] != token) {
            return errLine(id, "unauthorized: bad or missing token")
        }
        val verb = msg["verb"] as? String ?: return errLine(id, "missing verb")

        @Suppress("UNCHECKED_CAST")
        val args = msg["args"] as? Map<String, Any?> ?: emptyMap()
        return try {
            val result = synchronized(handlerLock) { handler(verb, args) }
            okLine(id, result)
        } catch (e: CtlException) {
            errLine(id, e.message ?: "error")
        } catch (e: Throwable) {
            errLine(id, e.cause?.message ?: e.message ?: e.javaClass.simpleName)
        }
    }

    private fun okLine(id: Any?, result: Any?): String {
        val obj = LinkedHashMap<String, Any?>()
        obj["ok"] = true
        obj["result"] = result
        if (id != null) obj["id"] = id
        return Json.write(obj)
    }

    private fun errLine(id: Any?, message: String): String {
        val obj = LinkedHashMap<String, Any?>()
        obj["ok"] = false
        obj["error"] = message
        if (id != null) obj["id"] = id
        return Json.write(obj)
    }

    private fun thread(name: String, body: () -> Unit): Thread =
        Thread(body, name).apply {
            isDaemon = true
            start()
        }

    override fun dispose() {
        closed = true
        runCatching { listener.close() }
        Discovery.delete(dir, instance)
    }

    private companion object {
        /** 32-hex token, same shape as ctlcore's `mint_token`. */
        fun mintToken(): String {
            val bytes = ByteArray(16)
            SecureRandom().nextBytes(bytes)
            return bytes.joinToString("") { "%02x".format(it) }
        }
    }
}
