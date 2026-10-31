package com.enigmacurry.rpg_vox_scribe

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * [Para.rows]: what the transcript draws, and where each row sits in the
 * paragraph. The offsets are what playing a word and cutting a selection
 * go by, so they are checked rather than eyeballed.
 */
class RowsTest {
    private fun para(text: String, readings: List<Reading>) = Para(
        id = "p",
        clips = listOf(ClipUi(text, partial = false)),
        readings = readings,
        translated = if (readings.isEmpty()) null else "done",
    )

    @Test
    fun a_paragraph_with_no_reading_is_one_row() {
        val rows = para("Да, конечно.", emptyList()).rows()
        assertEquals(listOf(Row("Да, конечно.", null, 0, 0)), rows)
    }

    @Test
    fun each_sentence_keeps_its_own_reading() {
        val p = para(
            "Да, конечно. Крыша плоская.",
            listOf(
                Reading("Да, конечно.", "Yes, of course."),
                Reading("Крыша плоская.", "The roof is flat."),
            ),
        )
        assertEquals(
            listOf(
                Row("Да, конечно.", "Yes, of course.", 0, 0),
                Row("Крыша плоская.", "The roof is flat.", 2, 13),
            ),
            p.rows(),
        )
    }

    /**
     * The sentence being spoken now is a row with nothing under it: the
     * reading runs a sentence behind on purpose, and the words have to
     * be on screen before it arrives.
     */
    @Test
    fun the_sentence_still_being_spoken_is_a_row_of_its_own() {
        val p = para(
            "Да, конечно. Крыша плоская",
            listOf(Reading("Да, конечно.", "Yes, of course.")),
        )
        assertEquals(
            listOf(
                Row("Да, конечно.", "Yes, of course.", 0, 0),
                Row("Крыша плоская", null, 2, 13),
            ),
            p.rows(),
        )
    }

    /** A reading whose sentence a later pass rewrote is dropped, not misplaced. */
    @Test
    fun a_reading_that_no_longer_matches_what_was_said_is_dropped() {
        val p = para(
            "Да, конечно! Крыша плоская.",
            listOf(
                Reading("Да, конечно.", "Yes, of course."),
                Reading("Крыша плоская.", "The roof is flat."),
            ),
        )
        assertEquals(
            listOf(
                Row("Да, конечно!", null, 0, 0),
                Row("Крыша плоская.", "The roof is flat.", 2, 13),
            ),
            p.rows(),
        )
    }

    /** Every row's text is the paragraph's own, at the offset it says. */
    @Test
    fun the_offsets_point_at_the_rows_own_words() {
        val p = para(
            "Один. Два. Три.",
            listOf(Reading("Один.", "One."), Reading("Три.", "Three.")),
        )
        val text = p.text
        for (row in p.rows()) {
            assertEquals(row.said, text.substring(row.at, row.at + row.said.length))
            val before = text.take(row.at).split(' ').count { it.isNotEmpty() }
            assertEquals("words before ${row.said}", before, row.firstWord)
        }
    }
}
