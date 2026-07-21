package dev.yeroo.offxy.ctl

import dev.yeroo.offxy.editor.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.Socket
import java.nio.file.Files

/** Wire-level tests against a fake handler — no IDE, no engine. */
class CtlServerTest {
    private fun tempDir() = Files.createTempDirectory("offxy-ctl-test")

    private fun request(port: Int, line: String): Map<String, Any?> =
        Socket("127.0.0.1", port).use { s ->
            s.getOutputStream().write((line + "\n").toByteArray())
            s.getOutputStream().flush()
            val reply = BufferedReader(InputStreamReader(s.getInputStream())).readLine()
            @Suppress("UNCHECKED_CAST")
            Json.parse(reply) as Map<String, Any?>
        }

    @Test
    fun discoveryFileTokenAndIdEcho() {
        val dir = tempDir()
        val server = CtlServer("docxy-jetbrains-t-1", dir) { verb, args ->
            linkedMapOf("verb" to verb, "echo" to args["x"])
        }
        try {
            server.start()
            val disc = Files.readString(Discovery.path(dir, "docxy-jetbrains-t-1"))
            @Suppress("UNCHECKED_CAST")
            val d = Json.parse(disc) as Map<String, Any?>
            assertEquals("docxy-jetbrains-t-1", d["instance"])
            assertEquals(server.port.toLong(), d["port"])
            assertEquals(server.token, d["token"])
            assertTrue(d.containsKey("pid"))

            // Bad token: exact ctlcore error string.
            val bad = request(server.port, """{"token":"nope","verb":"doc.path","id":9}""")
            assertEquals(false, bad["ok"])
            assertEquals("unauthorized: bad or missing token", bad["error"])
            assertEquals(9L, bad["id"])

            // Good token routes and echoes id + args.
            val ok = request(
                server.port,
                """{"token":"${server.token}","verb":"doc.read","args":{"x":42},"id":7}""",
            )
            assertEquals(true, ok["ok"])
            assertEquals(7L, ok["id"])
            @Suppress("UNCHECKED_CAST")
            val result = ok["result"] as Map<String, Any?>
            assertEquals("doc.read", result["verb"])
            assertEquals(42L, result["echo"])

            // Missing verb.
            val missing = request(server.port, """{"token":"${server.token}"}""")
            assertEquals("missing verb", missing["error"])

            // Handler CtlException becomes ok:false with the message.
            val failing = CtlServer("docxy-jetbrains-t-2", dir) { _, _ ->
                throw CtlException("not yet implemented")
            }
            failing.start()
            try {
                val err = request(failing.port, """{"token":"${failing.token}","verb":"doc.outline"}""")
                assertEquals(false, err["ok"])
                assertEquals("not yet implemented", err["error"])
            } finally {
                failing.dispose()
            }
        } finally {
            server.dispose()
        }
        assertFalse("dispose must remove discovery", Files.exists(Discovery.path(dir, "docxy-jetbrains-t-1")))
    }

    @Test
    fun refreshTimerRestoresSweptDiscovery() {
        val dir = tempDir()
        val server = CtlServer("docxy-jetbrains-r-1", dir, refreshMillis = 50) { _, _ -> null }
        try {
            server.start()
            val path = Discovery.path(dir, "docxy-jetbrains-r-1")
            assertTrue(Files.exists(path))
            Files.delete(path)
            val deadline = System.currentTimeMillis() + 2000
            while (!Files.exists(path) && System.currentTimeMillis() < deadline) Thread.sleep(20)
            assertTrue("swept discovery file was not rewritten", Files.exists(path))
        } finally {
            server.dispose()
        }
    }

    @Test
    fun invalidJsonAnswersInsteadOfHanging() {
        val dir = tempDir()
        val server = CtlServer("docxy-jetbrains-j-1", dir) { _, _ -> null }
        try {
            server.start()
            val reply = request(server.port, "this is not json")
            assertEquals(false, reply["ok"])
            assertTrue((reply["error"] as String).startsWith("invalid json"))
        } finally {
            server.dispose()
        }
    }
}
