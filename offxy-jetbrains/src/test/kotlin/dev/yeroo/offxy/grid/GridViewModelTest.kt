package dev.yeroo.offxy.grid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class GridViewModelTest {
    private fun view(extra: String): GridViewModel = GridViewModel(
        """{"sheets":["Sheet1"],"active":0,"dims":{"rows":1,"cols":1},"colw":[],"cells":[],""" +
            """"sel":{"r":0,"c":0,"r2":0,"c2":0},"cur":{"ref":"A1","src":"=A1"},""" +
            """"dirty":true,"edits":1$extra}""",
    )

    @Test
    fun circularRefIsParsedAndNoted() {
        val v = view(""","circular":"'My Data'!A1"""")
        assertEquals("'My Data'!A1", v.circular)
        assertEquals("Circular References: 'My Data'!A1", v.circularNote)
    }

    @Test
    fun noCircularKeyMeansNoNote() {
        val v = view("")
        assertNull(v.circular)
        assertNull(v.circularNote)
    }
}
