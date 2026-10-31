package com.enigmacurry.rpg_vox_scribe

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
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
        assertEquals(listOf(Row("Да, конечно.", null)), rows)
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
                Row("Да, конечно.", "Yes, of course."),
                Row("Крыша плоская.", "The roof is flat."),
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
                Row("Да, конечно.", "Yes, of course."),
                Row("Крыша плоская", null),
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
                Row("Да, конечно!", null),
                Row("Крыша плоская.", "The roof is flat."),
            ),
            p.rows(),
        )
    }
}

/**
 * Which paragraphs are shown as themselves and which as their reading.
 * [Para.own] is the whole of that decision, and the one thing the screen
 * must never do is show a language the reader doesn't read.
 */
class OwnTest {
    private fun para(text: String, translated: String?, readings: List<Reading> = emptyList()) = Para(
        id = "p",
        clips = listOf(ClipUi(text, partial = false)),
        readings = readings,
        translated = translated,
    )

    @Test
    fun a_paragraph_in_your_own_language_is_itself() {
        // Translation off, or nothing to translate.
        assertTrue(para("Yes, of course.", null).own)
        assertTrue(para("Yes, of course.", "native").own)
    }

    @Test
    fun a_translated_paragraph_is_shown_as_its_reading() {
        val p = para(
            "Да, конечно.",
            "done",
            listOf(Reading("Да, конечно.", "Yes, of course.")),
        )
        assertFalse(p.own)
        assertEquals(listOf("Yes, of course."), p.readings.map { it.text })
    }

    /** Nothing more is coming, so what was said is better than nothing. */
    @Test
    fun a_translation_that_failed_falls_back_to_what_was_said() {
        assertTrue(para("Да, конечно.", "failed").own)
    }

    /**
     * Being read for the first time: not the reader's language, and no
     * reading yet, which the screen shows as an ellipsis rather than as
     * the other language.
     */
    @Test
    fun a_paragraph_still_being_read_is_not_shown_as_what_was_said() {
        val p = para("Да, конечно.", "running")
        assertFalse(p.own)
        assertTrue(p.readings.isEmpty())
        assertTrue(p.rows().all { it.reading == null })
    }
}
