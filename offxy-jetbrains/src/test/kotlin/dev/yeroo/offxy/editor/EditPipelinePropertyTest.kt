package dev.yeroo.offxy.editor

import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import dev.yeroo.offxy.engine.ChicoryEngine
import kotlin.random.Random

/**
 * The divergence guard: random native edits through the pipeline must keep
 * Document text and engine render identical after every reconcile. Failures
 * print the seed and the script for replay.
 */
class EditPipelinePropertyTest : BasePlatformTestCase() {
    private fun openEditor(md: String): DocxFileEditor {
        val bytes = ChicoryEngine.fromMarkdown(md)
        return DocxEditorProvider().createEditor(project, BinaryLightVirtualFile("p.docx", bytes))
            as DocxFileEditor
    }

    private fun assertInSync(editor: DocxFileEditor, context: String) {
        editor.flushEdits()
        val docText = editor.view!!.document.text
        val engineText = ViewModel(editor.engine().render()).text
        assertEquals("diverged after $context", engineText, docText)
    }

    fun testTypingEnterBackspaceDeterministic() {
        val editor = openEditor("# Head\n\nAlpha beta gamma.\n\nSecond paragraph.\n")
        try {
            val doc = editor.view!!.document
            val insertAt = doc.text.indexOf("beta")
            WriteCommandAction.runWriteCommandAction(project) { doc.insertString(insertAt, "XY") }
            assertInSync(editor, "insert")
            assertTrue(ViewModel(editor.engine().render()).text.contains("XYbeta"))

            val enterAt = doc.text.indexOf("gamma")
            WriteCommandAction.runWriteCommandAction(project) { doc.insertString(enterAt, "\n") }
            assertInSync(editor, "enter")

            val delAt = doc.text.indexOf("XY")
            WriteCommandAction.runWriteCommandAction(project) { doc.deleteString(delAt, delAt + 2) }
            assertInSync(editor, "delete")
            assertFalse(ViewModel(editor.engine().render()).text.contains("XY"))
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testMultiLinePasteReplaysAsOnePaste() {
        val editor = openEditor("Start here.\n")
        try {
            val doc = editor.view!!.document
            val at = doc.text.indexOf("here")
            WriteCommandAction.runWriteCommandAction(project) {
                doc.insertString(at, "one\ntwo\nthree ")
            }
            assertInSync(editor, "multi-line paste")
            val text = ViewModel(editor.engine().render()).text
            assertTrue("paste content missing: $text", text.contains("two"))
        } finally {
            Disposer.dispose(editor)
        }
    }

    fun testRandomEditScriptsStayInSync() {
        val seed = Random.nextLong()
        val rnd = Random(seed)
        val editor = openEditor(
            "# Title\n\nThe quick brown fox jumps over the lazy dog again and again.\n\n" +
                "- first item\n- second item\n\nAnother paragraph with plenty of text to edit.\n",
        )
        try {
            val doc = editor.view!!.document
            val script = StringBuilder()
            repeat(80) { step ->
                val view = editor.view!!.currentView()!!
                // Collect editable absolute ranges wide enough to edit inside.
                val targets = ArrayList<IntRange>()
                for ((line, segs) in view.segs.withIndex()) {
                    for (seg in segs) {
                        if (seg.last - seg.first >= 2) {
                            val a = view.gridToOffset(line, seg.first)
                            val b = view.gridToOffset(line, seg.last)
                            if (b > a + 1) targets.add(a until b)
                        }
                    }
                }
                if (targets.isEmpty()) return@repeat
                val t = targets[rnd.nextInt(targets.size)]
                val op = rnd.nextInt(10)
                when {
                    op < 6 -> {
                        val off = t.first + rnd.nextInt(t.last - t.first + 1)
                        val text = List(1 + rnd.nextInt(3)) { ('a' + rnd.nextInt(26)) }.joinToString("")
                        script.append("insert@$off:$text; ")
                        WriteCommandAction.runWriteCommandAction(project) { doc.insertString(off, text) }
                    }
                    op < 9 -> {
                        val len = 1 + rnd.nextInt(2)
                        val start = t.first + rnd.nextInt((t.last - t.first + 1 - len).coerceAtLeast(1))
                        script.append("delete@$start+$len; ")
                        WriteCommandAction.runWriteCommandAction(project) {
                            doc.deleteString(start, (start + len).coerceAtMost(t.last + 1))
                        }
                    }
                    else -> {
                        val off = t.first + rnd.nextInt(t.last - t.first + 1)
                        script.append("enter@$off; ")
                        WriteCommandAction.runWriteCommandAction(project) { doc.insertString(off, "\n") }
                    }
                }
                editor.flushEdits()
                val docText = doc.text
                val engineText = ViewModel(editor.engine().render()).text
                assertEquals(
                    "diverged at step $step (seed=$seed, script=[$script])",
                    engineText, docText,
                )
            }
        } finally {
            Disposer.dispose(editor)
        }
    }
}
