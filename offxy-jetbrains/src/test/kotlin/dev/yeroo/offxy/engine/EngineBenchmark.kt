package dev.yeroo.offxy.engine

import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The spec's performance gate: warm per-keystroke latency on the largest
 * corpus document. Reported, not asserted (the 50 ms decision is a human
 * call) — but a wildly slow result fails loudly so CI surfaces it.
 */
class EngineBenchmark {
    @Test
    fun keystrokeLatencyOnLargestCorpusDoc() {
        val bytes = javaClass.getResourceAsStream("/fixtures/complex0.docx")!!.readBytes()
        ChicoryEngine().use { e ->
            assertTrue(e.open(bytes))
            e.cmd("width\t120")
            e.cmd("move\tdocend\t0")

            repeat(20) {
                e.cmd("insert\ta")
                e.cmd("backspace")
            }

            val samples = LongArray(200)
            for (i in samples.indices) {
                val t0 = System.nanoTime()
                e.cmd(if (i % 2 == 0) "insert\ta" else "backspace")
                samples[i] = System.nanoTime() - t0
            }
            samples.sort()
            val p50 = samples[samples.size / 2] / 1_000_000.0
            val p95 = samples[(samples.size * 95) / 100] / 1_000_000.0
            println("ENGINE BENCHMARK complex0.docx (${bytes.size} bytes): p50=%.2fms p95=%.2fms".format(p50, p95))

            // Sanity backstop only — the real gate (p95 <= 50ms) is judged from
            // the printed numbers per the spec.
            assertTrue("p95 ${"%.1f".format(p95)}ms is beyond usable", p95 < 500.0)
        }
    }
}
