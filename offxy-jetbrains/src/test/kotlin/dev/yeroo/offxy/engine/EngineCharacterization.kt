package dev.yeroo.offxy.engine

import org.junit.Assert.assertTrue
import org.junit.Test

/** Context for the performance gate: typical-size doc + long-warmup numbers. */
class EngineCharacterization {
    private fun bench(label: String, bytes: ByteArray, warmup: Int) {
        ChicoryEngine().use { e ->
            assertTrue(e.open(bytes))
            e.cmd("width\t120")
            e.cmd("move\tdocend\t0")
            repeat(warmup) {
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
            println("CHARACTERIZE $label (${bytes.size} bytes, warmup=$warmup): p50=%.2fms p95=%.2fms".format(p50, p95))
        }
    }

    @Test
    fun typicalDoc() =
        bench("sample.docx", javaClass.getResourceAsStream("/fixtures/sample.docx")!!.readBytes(), 20)

    @Test
    fun markdownMintedSmallDoc() =
        bench("small-minted", ChicoryEngine.fromMarkdown("# T\n\nOne paragraph.\n"), 20)

    @Test
    fun largestDocLongWarmup() =
        bench("complex0-warm500", javaClass.getResourceAsStream("/fixtures/complex0.docx")!!.readBytes(), 500)
}
