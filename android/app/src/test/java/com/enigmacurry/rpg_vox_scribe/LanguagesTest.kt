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
     * A language in the alphabet it is read into is offered now, and is
     * the whole point: German to an English reader was a pair the engine
     * refused until there was something that could tell the two apart.
     *
     * So the rule is no longer "different alphabets" but "the
     * classifier is among the models" -- and this is the test that would
     * have failed when it was the old rule, kept pointing the other way
     * so that the change is deliberate rather than a drift.
     */
    @Test
    fun a_language_in_the_alphabet_it_reads_into_is_offered() {
        val same = NATIVE_LANGUAGES.flatMap { into ->
            translatable(into).filter { it.script == into.script }
        }
        assertTrue("some language shares the alphabet it reads into", same.isNotEmpty())
    }

    /**
     * Every set of languages that shares an alphabet asks for the
     * classifier, because the engine refuses such a set without it
     * (`vox_transcribe::language::pair`). The model table is where that
     * can quietly stop being true: a direction added to `TRANSLATIONS`
     * whose language shares the reader's alphabet, with nothing asking
     * for `LANGID`, is a language that can be turned on and then fails
     * at load.
     */
    @Test
    fun the_classifier_is_asked_for_by_every_set_that_needs_it() {
        for (into in NATIVE_LANGUAGES) {
            for (from in translatable(into)) {
                val needed = from.script == into.script
                // What Scribe.wanted does, without an app to ask: the
                // chosen languages are the reader's and this one.
                val sharing = listOf(into, from).groupBy { it.script }.any { it.value.size > 1 }
                assertEquals(
                    "${from.label} into ${into.label}",
                    needed,
                    sharing,
                )
            }
        }
    }

    /**
     * Every code this app spells a model directory with is a code
     * `vox_transcribe::ct2` spells the same one with.
     *
     * The two tables are in two languages and cannot be one, and the way
     * they come apart is silent: the app downloads 80 MB into
     * `translate/nl-en`, `ct2::code("Dutch")` returns nothing, the
     * direction is left out of the translator, and Dutch is a language
     * you can turn on and that is never read. No error anywhere.
     *
     * So the Rust table is read as text. Fragile against a reformat of
     * `CODES` and worth it: the alternative is a pair of tables nothing
     * compares, which is what this is for.
     */
    @Test
    fun the_app_and_ct2_agree_on_every_code() {
        val source = ct2Source()
        // `("Dutch", "nl"),` → Dutch to nl.
        val entry = Regex("""\(\s*"([A-Za-z]+)"\s*,\s*"([a-z]+)"\s*\)""")
        val ct2 = entry.findAll(source.substringAfter("const CODES").substringBefore("];"))
            .associate { it.groupValues[1] to it.groupValues[2] }
        assertTrue("CODES parsed from ct2.rs: $ct2", ct2.size > 10)
        // Every language in the table, with no "is it used" escape: a
        // language in this enum is a language the app offers, and asking
        // `translation` whether it is used would be asking a question
        // that runs through `code` -- the very thing under test. A wrong
        // code makes no direction match, `translation` returns null, and
        // the check would skip exactly the case it is here to catch.
        for (l in Language.entries) {
            assertEquals("${l.label}: this app's code against ct2's", ct2[l.label], l.code)
        }
    }

    /** `vox_transcribe/src/ct2.rs`, found by walking up from the tests. */
    private fun ct2Source(): String {
        var dir: java.io.File? = java.io.File("").absoluteFile
        while (dir != null) {
            val candidate = java.io.File(dir, "crates/vox_transcribe/src/ct2.rs")
            if (candidate.isFile) return candidate.readText()
            dir = dir.parentFile
        }
        error("no crates/vox_transcribe/src/ct2.rs at or above ${java.io.File("").absolutePath}")
    }

    /** The classifier is one file, in a directory of its own. */
    @Test
    fun the_classifier_is_a_model_like_the_others() {
        assertEquals("langid", LANGID.dir)
        assertEquals(listOf("lid.176.ftz"), LANGID.installed.toList())
        // Its own directory and not the root of the models store: it is
        // deleted by removing `dir`, which at the root would take every
        // other model with it.
        assertTrue("the classifier has a directory to be deleted", LANGID.dir.isNotEmpty())
        // Under a megabyte, which is why nothing is asked before it is
        // fetched. A size that drifted far from this would mean the
        // Models page is quoting the wrong number.
        assertTrue("${LANGID.approxBytes} bytes", LANGID.approxBytes < 2_000_000)
    }

    /**
     * The Models page can name and size every model it lists, and lists
     * each one once. A model in the catalogue twice is two rows with two
     * Delete buttons for one directory.
     */
    @Test
    fun the_catalogue_lists_every_model_once() {
        val rows = catalogue()
        val dirs = rows.map { it.model.dir }
        assertEquals("a model is listed twice", dirs.distinct().size, dirs.size)
        for (m in listOf(PARAKEET, ZIPFORMER_EN, ZIPFORMER_RU, LANGID)) {
            assertTrue("${m.dir} is in the catalogue", m.dir in dirs)
        }
        // Only what nothing works without is undeletable.
        assertEquals(
            listOf(PARAKEET.dir, ZIPFORMER_EN.dir).sorted(),
            rows.filter { it.required }.map { it.model.dir }.sorted(),
        )
        for (row in rows) {
            assertTrue("${row.model.dir} has a size", row.model.approxBytes > 0)
            assertTrue("${row.model.dir} has files", row.model.installed.isNotEmpty())
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
