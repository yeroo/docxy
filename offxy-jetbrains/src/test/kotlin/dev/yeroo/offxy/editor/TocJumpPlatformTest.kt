package dev.yeroo.offxy.editor

import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.BinaryLightVirtualFile
import com.intellij.testFramework.fixtures.BasePlatformTestCase

/** Ctrl+click on a TOC entry jumps to its heading (engine `goto` + caret). */
class TocJumpPlatformTest : BasePlatformTestCase() {
    fun testTocEntryJumpsToItsBookmark() {
        val bytes = javaClass.getResourceAsStream("/fixtures/TOC2.docx")!!.readBytes()
        val editor = DocxEditorProvider().createEditor(
            project, BinaryLightVirtualFile("toc.docx", bytes),
        ) as DocxFileEditor
        try {
            val view = editor.view!!
            val v = view.currentView()!!

            // Find a TOC entry: a span linking to an internal #anchor.
            var anchorLink: String? = null
            var linkLine = -1
            outer@ for ((i, line) in v.lines.withIndex()) {
                for (sp in line) {
                    if (sp.link?.startsWith("#") == true) {
                        anchorLink = sp.link
                        linkLine = i
                        break@outer
                    }
                }
            }
            assertNotNull("TOC2.docx should render internal #anchor links", anchorLink)

            editor.followLink(anchorLink!!)
            val landedLine = view.editor.caretModel.logicalPosition.line
            assertTrue(
                "caret should land past the TOC entry (link at $linkLine, landed $landedLine)",
                landedLine > linkLine,
            )
            assertFalse("goto must not dirty the document", editor.isModified)

            // linkAt resolves the same span by offset (the mouse path's lookup).
            val offset = v.gridToOffset(linkLine, v.segs[linkLine].first().first)
            assertEquals(anchorLink, v.linkAt(offset))
        } finally {
            Disposer.dispose(editor)
        }
    }
}
