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

    /** Reading English, with Russian being listened out for. */
    private fun shown(p: Para) = p.own(translating = true, native = Script.LATIN)

    @Test
    fun a_paragraph_in_your_own_language_is_itself() {
        assertTrue(shown(para("Yes, of course, that is right.", "native")))
    }

    @Test
    fun a_translated_paragraph_is_shown_as_its_reading() {
        val p = para(
            "Да, конечно.",
            "done",
            listOf(Reading("Да, конечно.", "Yes, of course.")),
        )
        assertFalse(shown(p))
        assertEquals(listOf("Yes, of course."), p.readings.map { it.text })
    }

    /** Nothing more is coming, so what was said is better than nothing. */
    @Test
    fun a_translation_that_failed_falls_back_to_what_was_said() {
        assertTrue(shown(para("Да, конечно.", "failed")))
    }

    /** With nothing being translated, every paragraph is itself. */
    @Test
    fun with_translation_off_everything_is_shown_as_it_was_said() {
        for (state in listOf(null, "running", "native", "done")) {
            assertTrue(
                "state $state",
                para("Да, конечно.", state).own(translating = false, native = Script.LATIN),
            )
        }
    }

    /**
     * The state the engine has not answered yet, which is every
     * paragraph while pass 1 is typing it: the alphabet decides, and
     * this is one of the two cases that put Russian on the screen.
     */
    @Test
    fun a_paragraph_the_engine_has_not_ruled_on_goes_by_its_alphabet() {
        // Being typed by pass 1 in Russian: not shown, whatever the
        // engine has or hasn't said yet.
        assertFalse(shown(para("Да, конечно, это правда", null)))
        assertFalse(shown(para("Да, конечно, это правда", "running")))
        // Being typed in English: shown, as it always was.
        assertTrue(shown(para("Yes, of course, that is right", null)))
        assertTrue(shown(para("Yes, of course, that is right", "running")))
    }

    /**
     * "Nothing to translate" is what the engine says of a paragraph
     * whose alphabet it could not read, as well as of one in your own
     * language -- and the second case was showing Russian. The alphabet
     * is asked again here rather than taken on trust.
     */
    @Test
    fun nothing_to_translate_is_not_taken_to_mean_your_own_language() {
        assertFalse(shown(para("Да, конечно, это правда", "native")))
        assertTrue(shown(para("Yes, of course, that is right", "native")))
    }

    /**
     * Text too short or too mixed to place is shown: "OK." is as likely
     * as "Да.", and hiding every short utterance to catch the second
     * would lose the first.
     */
    @Test
    fun text_whose_alphabet_cannot_be_read_is_shown() {
        assertTrue(shown(para("Да", null)))
        assertTrue(shown(para("OK", null)))
        assertTrue(shown(para("Да", "native")))
    }

    /** A word of one language in a line of another does not decide it. */
    @Test
    fun a_stray_word_does_not_decide_the_alphabet() {
        assertTrue(shown(para("Yes of course, да, that is right", null)))
        assertFalse(shown(para("Да, конечно, это правда, OK", null)))
    }
}
