package dev.cdash.watch

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NotesTest {
    @Test fun backoffDoublesToFiveMinutes() {
        assertEquals(5_000L, FIRST_DELAY_MS); assertEquals(10_000L, nextDelay(5_000)); assertEquals(300_000L, nextDelay(200_000))
    }
    @Test fun onlyDataLinesCarryNotes() {
        assertEquals("{\"s\":\"cdash-a\"}", dataOf("data: {\"s\":\"cdash-a\"}")); assertNull(dataOf(": keep")); assertNull(dataOf(""))
    }
    @Test fun textsAndChannels() {
        assertEquals("Finished", textFor("done")); assertEquals("Stopped with an error", textFor("error"))
        assertEquals("Needs your answer", textFor("ask")); assertNull(textFor("later"))
        assertEquals(CHANNEL_REPLIES, channelFor("ask")); assertEquals(CHANNEL_REPLIES, channelFor("error")); assertEquals(CHANNEL_DONE, channelFor("done"))
    }
}
