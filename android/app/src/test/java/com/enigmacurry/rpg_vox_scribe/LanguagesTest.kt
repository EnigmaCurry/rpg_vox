package com.enigmacurry.rpg_vox_scribe

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The model table and the language rules, on the JVM: the part of this
 * app that can be checked without a phone, and the part whose mistakes
 * look like a crash on startup or a recording that refuses to begin.
 */
class LanguagesTest {
    /**
     * [Language] is touched first, as the app touches it (`Scribe.native`
     * is a `Language`), and that is the order that broke: `Language`'s
     * initializer reads the models beside it, so a top-level `val` there
     * that read `Language.entries` back left the two half-initialized and
     * `entries` null. It crashed before the first screen, with an
     * `ExceptionInInitializerError` naming neither cause.
     */
    @Test
    fun models_and_languages_do_not_initialize_in_a_circle() {
        assertNotNull("the pass-1 model for English", Language.ENGLISH.streaming)
        assertNotNull("the pass-1 model for Russian", Language.RUSSIAN.streaming)
        assertEquals(NATIVE_LANGUAGES, listOf(Language.ENGLISH, Language.RUSSIAN))
        assertTrue(BUNDLES.isNotEmpty())
    }

    /** A language can be read in exactly when pass 1 has a model for it. */
    @Test
    fun the_languages_offered_to_read_in_are_the_ones_pass_1_can_hear() {
        assertEquals(
            Language.entries.filter { it.streaming != null },
            NATIVE_LANGUAGES,
        )
    }

    /**
     * No language is offered in the alphabet it would be read into. That
     * is the rule the engine enforces by refusing the pair -- a paragraph
     * is assigned a language by its alphabet, so one in the alphabet you
     * read could never be told from your own -- and the model table is
     * where it can quietly stop being true. Adding `de-en` to
     * `TRANSLATIONS`, say, would offer German to an English reader and
     * then fail at load.
     */
    @Test
    fun nothing_is_offered_in_the_alphabet_it_reads_into() {
        for (into in NATIVE_LANGUAGES) {
            for (from in translatable(into)) {
                assertTrue(
                    "${from.label} is ${from.script}, like ${into.label}",
                    from.script != into.script,
                )
            }
        }
    }

    /** Every offered language has the model the page would download. */
    @Test
    fun every_offered_language_has_a_model_named_the_way_ct2_looks_for_it() {
        var offered = 0
        for (into in NATIVE_LANGUAGES) {
            for (from in translatable(into)) {
                val model = translation(from, into)
                assertNotNull("${from.code}-${into.code}", model)
                // vox_transcribe::ct2 looks for `translate/<from>-<into>`
                // with the opus-mt codes, and reads it with a
                // tokenizer.json written after the download.
                assertEquals("translate/${from.code}-${into.code}", model!!.dir)
                assertTrue(model.tokenizer)
                assertTrue("source.spm" in model.files)
                assertTrue(TOKENIZER in model.installed)
                offered++
            }
        }
        assertTrue("some language is offered", offered > 0)
    }
}
