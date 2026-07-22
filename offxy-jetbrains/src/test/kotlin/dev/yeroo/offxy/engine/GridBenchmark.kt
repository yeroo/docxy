package dev.yeroo.offxy.engine

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** Viewport + edit latency on the largest local corpus workbook. The file is
 *  corpus-external (not committed), so CI prints a SKIP instead of failing. */
class GridBenchmark {
    @Test
    fun viewAndSetLatencyOnLargestCorpusWorkbook() {
        val big = File("../corpus/xlsx-ext/openoffice/test/testgui/data/pvt/complex_29s.xlsx")
        if (!big.exists()) {
            println("GRID BENCHMARK SKIP: ${big.path} not present (local corpus only)")
            return
        }
        GridEngine().use { e ->
            assertTrue(e.open(big.readBytes()))
            e.cmd("view\t0\t0\t0\t40\t20")
            repeat(20) { e.cmd("view\t0\t${it * 5}\t0\t40\t20") }

            fun bench(label: String, op: (Int) -> String) {
                val samples = LongArray(100)
                for (i in samples.indices) {
                    val t0 = System.nanoTime()
                    e.cmd(op(i))
                    samples[i] = System.nanoTime() - t0
                }
                samples.sort()
                val p50 = samples[50] / 1_000_000.0
                val p95 = samples[95] / 1_000_000.0
                println("GRID BENCHMARK $label (${big.length()} bytes): p50=%.2fms p95=%.2fms".format(p50, p95))
            }
            bench("view-scroll") { "view\t0\t${(it * 7) % 200}\t0\t40\t20" }
            bench("set+recalc") { "set\t${200 + (it % 20)}\t0\t${it}" }
        }
    }
}
