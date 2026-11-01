package com.enigmacurry.rpg_vox_scribe

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The translations, against the language the app was written in.
 *
 * A missing name is a crash on the screen that wanted it, and a format
 * argument that moved or changed type is an `IllegalFormatException` at
 * the moment the sentence is said -- both of which happen in one
 * language and not the other, which is to say on somebody else's phone.
 * So they are checked here, where a translation that has drifted fails
 * the build instead.
 *
 * Nothing here judges the words. It checks the shape: the same names,
 * the same arguments in the same places, enough plural forms for the
 * language, and a `locales_config.xml` that lists every translation
 * there is -- without which Android 13 and newer never offer the app a
 * language of its own.
 */
class StringsTest {
    /**
     * Plural forms a language needs, beyond the ones English uses.
     * Russian says "1 абзац", "2 абзаца" and "5 абзацев", so a plural
     * with only `one` and `other` is wrong there in most of the
     * sentences it appears in. Android falls back to `other` for a form
     * that isn't given, silently.
     */
    private val pluralForms = mapOf(
        "ru" to setOf("one", "few", "many", "other"),
    )

    @Test
    fun every_translation_says_everything_the_default_does() {
        val default = strings(File(res, "values/strings.xml"))
        val translatable = default.filterValues { it.translatable }
        assertTrue("values/strings.xml has strings in it", translatable.size > 50)
        for ((code, file) in translations()) {
            val other = strings(file)
            assertEquals(
                "$code: names missing from values-$code/strings.xml",
                emptySet<String>(),
                translatable.keys - other.keys,
            )
            assertEquals(
                "$code: names with nothing in values/strings.xml to translate",
                emptySet<String>(),
                other.keys - default.keys,
            )
            for (name in other.keys) {
                assertTrue(
                    "$code: $name is marked translatable=\"false\"",
                    default.getValue(name).translatable,
                )
            }
        }
    }

    @Test
    fun every_translation_takes_the_same_arguments_in_the_same_order() {
        val default = strings(File(res, "values/strings.xml"))
        for ((code, file) in translations()) {
            for ((name, res) in strings(file)) {
                val want = default.getValue(name).args
                assertEquals("$code: $name takes different arguments", want, res.args)
            }
        }
    }

    @Test
    fun every_translation_has_the_plural_forms_its_language_uses() {
        val default = strings(File(res, "values/strings.xml"))
        for ((code, file) in translations()) {
            val wanted = pluralForms[code] ?: continue
            for ((name, res) in strings(file)) {
                if (!default.getValue(name).plural) continue
                assertEquals("$code: $name is missing a plural form", wanted, res.quantities)
            }
        }
    }

    /**
     * The per-app language picker Android 13 and newer put in Settings
     * is driven by this list and nothing else, so a translation added
     * without a line in it is a translation only a reader who switches
     * their whole phone will ever see.
     */
    @Test
    fun locales_config_lists_the_default_language_and_every_translation() {
        val listed = parse(File(res, "xml/locales_config.xml"))
            .getElementsByTagName("locale")
            .let { nodes -> (0 until nodes.length).map { (nodes.item(it) as Element).getAttribute("android:name") } }
        assertEquals(
            "xml/locales_config.xml does not match the values-* directories",
            listOf(DEFAULT_LANGUAGE) + translations().keys.sorted(),
            listed,
        )
    }

    /** `values-<code>/strings.xml`, by language code. */
    private fun translations(): Map<String, File> =
        res.listFiles().orEmpty()
            .filter { it.name.startsWith("values-") }
            .mapNotNull { dir -> File(dir, "strings.xml").takeIf { it.isFile }?.let { dir.name.substringAfter('-') to it } }
            .sortedBy { it.first }
            .toMap()

    /** One `<string>` or `<plurals>`: what the build and the formatter care about. */
    private data class Res(
        val translatable: Boolean,
        val plural: Boolean,
        /** `%1$s`, `%2$d`, … in the order they are numbered. */
        val args: List<String>,
        /** A plural's `quantity` attributes; empty for a plain string. */
        val quantities: Set<String>,
    )

    private fun strings(file: File): Map<String, Res> {
        val root = parse(file)
        val out = LinkedHashMap<String, Res>()
        for (tag in listOf("string", "plurals")) {
            val nodes = root.getElementsByTagName(tag)
            for (i in 0 until nodes.length) {
                val e = nodes.item(i) as Element
                val plural = tag == "plurals"
                val items = if (plural) {
                    e.getElementsByTagName("item").let { n -> (0 until n.length).map { n.item(it) as Element } }
                } else {
                    listOf(e)
                }
                out[e.getAttribute("name")] = Res(
                    translatable = e.getAttribute("translatable") != "false",
                    plural = plural,
                    // Every form of a plural takes the same arguments,
                    // so the whole entry has one set of them.
                    args = items.flatMap { args(it.textContent) }.distinct().sorted(),
                    quantities = items.mapNotNull { it.getAttribute("quantity").takeIf(String::isNotEmpty) }.toSet(),
                )
            }
        }
        return out
    }

    /** `%1$s` and friends. `%%` is an escaped per cent and takes nothing. */
    private fun args(text: String): List<String> =
        Regex("""%(\d+)\$([a-z])""").findAll(text).map { it.value }.toList()

    private fun parse(file: File) =
        DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(file).documentElement

    private companion object {
        /** The language `values/` itself is written in. */
        const val DEFAULT_LANGUAGE = "en"

        /**
         * `src/main/res`, found by walking up from wherever the tests
         * were started: Gradle runs them in the module directory, and a
         * run from the repository root should work too.
         */
        val res: File = run {
            var dir: File? = File("").absoluteFile
            while (dir != null) {
                for (path in listOf("src/main/res", "android/app/src/main/res", "app/src/main/res")) {
                    val candidate = File(dir, path)
                    if (File(candidate, "values/strings.xml").isFile) return@run candidate
                }
                dir = dir.parentFile
            }
            error("no src/main/res at or above ${File("").absolutePath}")
        }
    }
}
