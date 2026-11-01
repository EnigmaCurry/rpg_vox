package com.enigmacurry.rpg_vox_scribe

import androidx.annotation.StringRes
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/**
 * One model, installed as `<models>/<dir>/`. Three kinds, because the
 * places these come from publish them differently: [Archive] is a
 * sherpa-onnx release `.tar.bz2` with the wanted files renamed out of
 * it, [Loose] is a Hugging Face repo whose files are downloaded one by
 * one, and [Single] is one file at a URL. Same sources and layout as
 * vox_scribe's `download-models`, so a desktop models directory would
 * work as-is.
 */
sealed interface Model {
    val dir: String

    /** Approximate download, for a progress bar before the server says. */
    val approxBytes: Long

    /** Files that must be there for it to count as installed. */
    val installed: Collection<String>
}

class Archive(
    override val dir: String,
    val stem: String,
    /** Archive path (below the top folder) to installed name. */
    val files: Map<String, String>,
    override val approxBytes: Long,
) : Model {
    val url = "$RELEASES/asr-models/$stem.tar.bz2"
    override val installed get() = files.values
}

/**
 * A Hugging Face repo's files, as `<path in the repo>` to
 * `<installed name>`. [tokenizer] writes a `tokenizer.json` from the
 * `source.spm` afterwards, which is what a Marian model is read with
 * (`Native.writeTokenizer`).
 */
class Loose(
    override val dir: String,
    val repo: String,
    val files: Map<String, String>,
    override val approxBytes: Long,
    val tokenizer: Boolean = false,
) : Model {
    fun url(path: String) = "https://huggingface.co/$repo/resolve/main/$path"
    override val installed get() = files.values + if (tokenizer) listOf(TOKENIZER) else emptyList()
}

/**
 * One file at one URL, in a directory of its own.
 *
 * A directory for a single file so that it is a model like the others:
 * one thing to install, find and delete whole. A file at the top of the
 * models directory would make deleting it a special case
 * ([ModelStore.remove] deletes `dir` and would take everything).
 */
class Single(
    override val dir: String,
    val url: String,
    val name: String,
    override val approxBytes: Long,
) : Model {
    override val installed get() = listOf(name)
}

private const val RELEASES = "https://github.com/k2-fsa/sherpa-onnx/releases/download"

/** The tokenizer vox_transcribe::ct2 reads a translation model with. */
const val TOKENIZER = "tokenizer.json"

/** Passes 2 and 3: the finished text, in any of 25 languages. */
val PARAKEET = Archive(
    dir = "parakeet-tdt-0.6b-v3",
    stem = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
    files = listOf("encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt")
        .associateWith { it },
    approxBytes = 487_000_000,
)

/**
 * Pass 1, the live text, in English. It is the one pass that needs a
 * model per language, because a streaming model has to be chosen before
 * anything has been said; [Language.streaming] is which.
 */
val ZIPFORMER_EN = Archive(
    dir = "streaming-zipformer",
    stem = "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17",
    files = mapOf(
        "encoder-epoch-99-avg-1.int8.onnx" to "encoder.onnx",
        "decoder-epoch-99-avg-1.onnx" to "decoder.onnx",
        "joiner-epoch-99-avg-1.int8.onnx" to "joiner.onnx",
        "tokens.txt" to "tokens.txt",
    ),
    approxBytes = 127_887_156,
)

/** Pass 1 in Russian: Vosk's small streaming Zipformer. */
val ZIPFORMER_RU = Loose(
    dir = "streaming-zipformer-ru",
    repo = "alphacep/vosk-model-small-streaming-ru",
    files = mapOf(
        "am-onnx/encoder.int8.onnx" to "encoder.onnx",
        "am-onnx/decoder.int8.onnx" to "decoder.onnx",
        "am-onnx/joiner.int8.onnx" to "joiner.onnx",
        "lang/tokens.txt" to "tokens.txt",
    ),
    approxBytes = 27_000_000,
)

/**
 * What tells two languages written in the same alphabet apart:
 * fastText's `lid.176`, quantized, 917 KB for 176 languages.
 *
 * It is what makes French readable by an English reader at all. Pass 2
 * hears twenty-five languages and never says which it heard, so a
 * paragraph's language is worked out from the text afterwards; the
 * alphabet does most of that for nothing, and this does the rest --
 * English against French, Russian against Ukrainian (see
 * `vox_transcribe::langid`).
 *
 * Small enough that the Models page offers it rather than making a
 * point of it, and [Scribe.wanted] asks for it whenever a chosen
 * language needs it.
 */
val LANGID = Single(
    dir = "langid",
    url = "https://dl.fbaipublicfiles.com/fasttext/supervised-models/lid.176.ftz",
    name = "lid.176.ftz",
    approxBytes = 938_013,
)

/** What the first run downloads: the two models nothing works without. */
val BUNDLES = listOf(ZIPFORMER_EN, PARAKEET)

/**
 * A language the app can read in or listen out for.
 *
 * [script] is the alphabet it is written in. It used to decide which
 * languages could be used together -- a paragraph's language was read
 * off its alphabet alone, so two languages sharing one could not be
 * told apart and the engine refused the pair. [LANGID] is what lifted
 * that, and the alphabet now decides something smaller and more useful:
 * whether the classifier is needed at all. Two languages in different
 * alphabets are still separated for nothing, by the text itself.
 *
 * Greek, Han, Hangul and Kana are still one [Script.OTHER] between
 * them. That is not a claim that they look alike -- it is that the
 * engine's `Script` enum has no finer distinction, so anything outside
 * Latin and Cyrillic is "neither of those" and the classifier settles
 * the rest, which it does well.
 *
 * [code] is how opus-mt spells it, for the translation model's name.
 * That is not always the ISO code the classifier is asked in (opus-mt
 * says `jap` where ISO 639-1 says `ja`), and the two never meet: the
 * classifier is reached through [label], which the engine maps to a
 * code of its own in `vox_transcribe::language`.
 *
 * [streaming] is the pass-1 bundle for reading in this language, null
 * for the languages that have none, which cannot be the one you read in.
 *
 * Two names, and both are needed. [label] is the engine's word for it
 * and is English whatever the screen is in: it is what `Native.load` is
 * told, what a paragraph's `heard_in` comes back as, and what the
 * chosen language is saved under, so a phone switched to another
 * language still loads the models it loaded yesterday. [nameRes] is
 * the name a reader is shown, in whichever language they are reading.
 */
enum class Language(
    val label: String,
    @StringRes val nameRes: Int,
    val code: String,
    val script: Script,
    val streaming: Model? = null,
) {
    ENGLISH("English", R.string.language_english, "en", Script.LATIN, ZIPFORMER_EN),
    RUSSIAN("Russian", R.string.language_russian, "ru", Script.CYRILLIC, ZIPFORMER_RU),
    UKRAINIAN("Ukrainian", R.string.language_ukrainian, "uk", Script.CYRILLIC),
    BULGARIAN("Bulgarian", R.string.language_bulgarian, "bg", Script.CYRILLIC),
    FRENCH("French", R.string.language_french, "fr", Script.LATIN),
    SPANISH("Spanish", R.string.language_spanish, "es", Script.LATIN),
    GERMAN("German", R.string.language_german, "de", Script.LATIN),
    ITALIAN("Italian", R.string.language_italian, "it", Script.LATIN),
    DUTCH("Dutch", R.string.language_dutch, "nl", Script.LATIN),
    DANISH("Danish", R.string.language_danish, "da", Script.LATIN),
    SWEDISH("Swedish", R.string.language_swedish, "sv", Script.LATIN),
    FINNISH("Finnish", R.string.language_finnish, "fi", Script.LATIN),
    HUNGARIAN("Hungarian", R.string.language_hungarian, "hu", Script.LATIN),
    CZECH("Czech", R.string.language_czech, "cs", Script.LATIN),
    SLOVAK("Slovak", R.string.language_slovak, "sk", Script.LATIN),
    POLISH("Polish", R.string.language_polish, "pl", Script.LATIN),
    LATVIAN("Latvian", R.string.language_latvian, "lv", Script.LATIN),
    ESTONIAN("Estonian", R.string.language_estonian, "et", Script.LATIN),
    MALTESE("Maltese", R.string.language_maltese, "mt", Script.LATIN),
    CHINESE("Chinese", R.string.language_chinese, "zh", Script.OTHER),
    JAPANESE("Japanese", R.string.language_japanese, "jap", Script.OTHER),
    KOREAN("Korean", R.string.language_korean, "ko", Script.OTHER);

    companion object {
        /** By the name vox_transcribe spells it with; English if unknown. */
        fun of(label: String?) = entries.firstOrNull { it.label == label } ?: ENGLISH
    }
}

/** The alphabets [Language] is sorted into. */
enum class Script(@StringRes val labelRes: Int) {
    LATIN(R.string.script_latin),
    CYRILLIC(R.string.script_cyrillic),
    OTHER(R.string.script_other),
}

/**
 * The alphabet [text] is written in, or null when there is too little of
 * it, or too much of a mixture, to say.
 *
 * The same rule and the same numbers as `vox_transcribe::language`,
 * which is what decides for real once pass 2 has the utterance: a
 * majority of the letters, and enough of them to believe. The app asks
 * it of a paragraph the engine has not ruled on yet -- while pass 1 is
 * still typing it -- to know whether it is text the reader can read.
 */
fun script(text: String): Script? {
    var latin = 0
    var cyrillic = 0
    var other = 0
    for (c in text) {
        if (!c.isLetter()) continue
        when (c) {
            in 'a'..'z', in 'A'..'Z' -> latin++
            in '\u0400'..'\u052f' -> cyrillic++
            // Latin letters with diacritics, which are Latin: é, ü, ø.
            in '\u00c0'..'\u024f' -> latin++
            else -> other++
        }
    }
    val total = latin + cyrillic + other
    // "OK" is two letters of Latin in a Russian conversation.
    if (total < 8) return null
    val (best, n) = listOf(
        Script.LATIN to latin,
        Script.CYRILLIC to cyrillic,
        Script.OTHER to other,
    ).maxBy { it.second }
    // Below a majority it is a line with a word of another language in
    // it, which is not the same as a line in that language.
    return best.takeIf { n.toFloat() / total >= 0.7f }
}

/**
 * The languages that can be read in: the ones with a pass-1 model.
 * Nothing else is offered, because the live text has to be in some
 * language and the finished text is already in all of them.
 *
 * A getter, and it has to be one: [Language]'s own initializer reads the
 * models above it, so a top-level `val` here that reads [Language] back
 * is a circle. Whichever of the two is touched first starts the other,
 * which then finds `Language.entries` still null and crashes the app on
 * startup with nothing to go on. Nothing else at the top level of this
 * file may mention [Language] either.
 */
val NATIVE_LANGUAGES: List<Language>
    get() = Language.entries.filter { it.streaming != null }

/**
 * The opus-mt model that reads `from` in `into`, or null when there is
 * none. Each is one direction of one pair, about 80 MB.
 *
 * The directions are listed rather than generated because the repos
 * exist for some pairs and not others, and a pair that is missing is
 * only found out about halfway through an 80 MB download.
 */
fun translation(from: Language, into: Language): Loose? {
    val pair = "${from.code}-${into.code}"
    if (pair !in TRANSLATIONS) return null
    return Loose(
        dir = "translate/$pair",
        // `-android` is the uploader's name for the conversion, not a
        // platform: these are the ordinary int8 CTranslate2 files, the
        // same ones the desktop downloads.
        repo = "manancode/opus-mt-$pair-ctranslate2-android",
        files = listOf("config.json", "shared_vocabulary.json", "source.spm", "model.bin")
            .associateWith { it },
        approxBytes = 81_000_000,
        tokenizer = true,
    )
}

/**
 * Directions with a published model, checked one by one against the
 * repo and its four files.
 *
 * Most of these are new, and they are what [LANGID] bought: every pair
 * written in the Latin alphabet was pointless to list while a
 * paragraph's language was read off its alphabet, because French could
 * not have been told from the English it would be read into. The
 * absences are the uploader's, not a rule -- Greek, Romanian, Croatian,
 * Slovenian, Lithuanian, Portuguese and `de-ru` have no readable repo.
 */
private val TRANSLATIONS = setOf(
    // Into English.
    "ru-en", "uk-en", "bg-en", "fr-en", "es-en", "de-en", "it-en", "nl-en",
    "da-en", "sv-en", "fi-en", "hu-en", "cs-en", "sk-en", "pl-en", "lv-en",
    "et-en", "mt-en", "zh-en", "jap-en", "ko-en",
    // Into Russian.
    "en-ru", "uk-ru", "fr-ru", "es-ru", "ko-ru",
)

/** Every language that can be translated into [into]. */
fun translatable(into: Language): List<Language> =
    Language.entries.filter { it != into && translation(it, into) != null }

/**
 * What kind of thing a model is, which is how the Models page groups
 * them. The order is the order the page lists them in: what nothing
 * works without, then what each language needs.
 */
enum class Kind(@StringRes val titleRes: Int) {
    SPEECH(R.string.models_kind_speech),
    LIVE(R.string.models_kind_live),
    READING(R.string.models_kind_reading),
}

/**
 * One row of the Models page: a model, what it is for, and what to call
 * it in a sentence a reader can act on.
 *
 * [required] is whether the app cannot run without it, which is the
 * only thing that makes a model undeletable. [wantedBy] is the language
 * whose being chosen asks for it, or null for the ones that are always
 * wanted -- the page uses it to say why something is there.
 */
class Listing(
    val model: Model,
    val kind: Kind,
    val required: Boolean = false,
    val wantedBy: Language? = null,
)

/**
 * Every model the app knows how to install, in the order the Models
 * page shows them.
 *
 * A function and not a `val`, for the reason given on
 * [NATIVE_LANGUAGES]: anything at the top level of this file that reads
 * [Language] back while [Language] is still initializing gets `entries`
 * as null and crashes the app before its first screen.
 */
fun catalogue(): List<Listing> {
    val rows = mutableListOf<Listing>()
    rows += Listing(PARAKEET, Kind.SPEECH, required = true)
    // Every pass-1 model, English first: it is required outright, since
    // it is downloaded on the first run and a sentence of English in
    // another language's conversation is the common case pass 1 moves
    // between languages for.
    rows += Listing(ZIPFORMER_EN, Kind.LIVE, required = true, wantedBy = Language.ENGLISH)
    for (l in NATIVE_LANGUAGES) {
        val m = l.streaming ?: continue
        if (m.dir == ZIPFORMER_EN.dir) continue
        rows += Listing(m, Kind.LIVE, wantedBy = l)
    }
    // The classifier, then one reading model per direction. Listed
    // under the same heading because they are wanted together: a
    // reading of French into English needs both, and a page that
    // separated them would be a page where turning on French leaves
    // something unexplained in another section.
    rows += Listing(LANGID, Kind.READING)
    for (into in NATIVE_LANGUAGES) {
        for (from in translatable(into)) {
            val m = translation(from, into) ?: continue
            rows += Listing(m, Kind.READING, wantedBy = from)
        }
    }
    return rows
}

sealed interface Step {
    data class Downloading(val bundle: String, val done: Long, val total: Long) : Step
    data class Unpacking(val bundle: String, val fraction: Float) : Step
    data class Converting(val bundle: String) : Step
}

/** App-private model store under `noBackupFilesDir/models`. */
class ModelStore(root: File) {
    val dir = File(root, "models")
    private val downloads = File(root, "downloads")

    fun installed(m: Model) = m.installed.all { File(dir, "${m.dir}/$it").isFile }

    /** The models nothing works without. */
    fun ready() = BUNDLES.all(::installed)

    /** Some download or unpack was left part-way. */
    fun started() = downloads.isDirectory && (downloads.list()?.isNotEmpty() == true)

    /** Bytes on disk for installed models. */
    fun size(): Long = dir.walkTopDown().filter { it.isFile }.sumOf { it.length() }

    /**
     * Bytes one model takes, as it actually is on disk rather than as
     * [Model.approxBytes] guessed. Zero when it isn't installed.
     *
     * The whole directory and not the listed files: a translation model
     * keeps a `tokenizer.json` written after the download, and a reader
     * being told what deleting something gives back should be told the
     * truth.
     */
    fun size(m: Model): Long =
        File(dir, m.dir).walkTopDown().filter { it.isFile }.sumOf { it.length() }

    /** Give back the space one model takes; it can be fetched again. */
    fun remove(m: Model) {
        File(dir, m.dir).deleteRecursively()
    }

    /**
     * Fetch and install every one of [models] that isn't there already.
     * Partial downloads are kept and resumed with an HTTP Range request
     * on the next call.
     */
    fun install(models: List<Model> = BUNDLES, progress: (Step) -> Unit) {
        downloads.mkdirs()
        dir.mkdirs()
        for (m in models) {
            if (installed(m)) continue
            when (m) {
                is Archive -> {
                    val archive = File(downloads, "${m.stem}.tar.bz2")
                    if (!archive.isFile) download(m.dir, m.url, m.approxBytes, archive, progress)
                    unpack(m, archive, progress)
                    archive.delete()
                }
                is Loose -> loose(m, progress)
                is Single -> single(m, progress)
            }
        }
        downloads.deleteRecursively()
    }

    /**
     * One file, downloaded beside its directory and moved in, like
     * [loose] and for the same reason: nothing that looks installed is
     * left behind by a fetch that stopped halfway.
     */
    private fun single(m: Single, progress: (Step) -> Unit) {
        val tmp = File(dir, "${m.dir.replace('/', '-')}.tmp")
        tmp.deleteRecursively()
        tmp.mkdirs()
        download(m.dir, m.url, m.approxBytes, File(tmp, m.name), progress)
        val final = File(dir, m.dir)
        final.parentFile?.mkdirs()
        final.deleteRecursively()
        if (!tmp.renameTo(final)) throw IOException("rename ${tmp.name}")
    }

    /**
     * Each file of a Hugging Face model, downloaded beside the final
     * directory and moved into place together, so an interrupted fetch
     * leaves nothing that looks installed.
     */
    private fun loose(m: Loose, progress: (Step) -> Unit) {
        val tmp = File(dir, "${m.dir.replace('/', '-')}.tmp")
        tmp.deleteRecursively()
        tmp.mkdirs()
        // One progress bar across the files, which are very different
        // sizes: model.bin is all but a fiftieth of a translation model.
        var before = 0L
        for ((path, name) in m.files) {
            val to = File(tmp, name)
            download(m.dir, m.url(path), m.approxBytes, to) { step ->
                progress(
                    when (step) {
                        is Step.Downloading ->
                            Step.Downloading(m.dir, before + step.done, m.approxBytes)
                        else -> step
                    },
                )
            }
            before += to.length()
        }
        if (m.tokenizer) {
            progress(Step.Converting(m.dir))
            val spm = File(tmp, "source.spm")
            if (!spm.isFile) throw IOException("${m.dir}: no source.spm to read the model with")
            runCatching { Native.writeTokenizer(spm.path, File(tmp, TOKENIZER).path) }
                .onFailure {
                    tmp.deleteRecursively()
                    throw IOException("${m.dir}: $TOKENIZER: ${it.message}", it)
                }
        }
        val final = File(dir, m.dir)
        final.parentFile?.mkdirs()
        final.deleteRecursively()
        if (!tmp.renameTo(final)) throw IOException("rename ${tmp.name}")
    }

    /** [url] to [to], resuming a `.part` beside it if there is one. */
    private fun download(
        name: String,
        url: String,
        approxBytes: Long,
        to: File,
        progress: (Step) -> Unit,
    ) {
        val part = File(to.parentFile, "${to.name}.part")
        var have = if (part.isFile) part.length() else 0L
        val conn = (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = 20_000
            readTimeout = 60_000
            instanceFollowRedirects = true
            if (have > 0) setRequestProperty("Range", "bytes=$have-")
        }
        try {
            val code = conn.responseCode
            when {
                code == HttpURLConnection.HTTP_PARTIAL -> {}
                code == HttpURLConnection.HTTP_OK -> have = 0 // server ignored Range
                code == 416 && have > 0 -> { // already complete
                    part.renameTo(to)
                    return
                }
                else -> throw IOException("HTTP $code for $url")
            }
            val len = conn.getHeaderFieldLong("Content-Length", -1)
            val total = if (len > 0) have + len else approxBytes
            conn.inputStream.use { input ->
                FileOutputStream(part, have > 0).use { out ->
                    val buf = ByteArray(256 * 1024)
                    var done = have
                    var shown = 0L
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        out.write(buf, 0, n)
                        done += n
                        if (done - shown > 1_000_000) {
                            shown = done
                            progress(Step.Downloading(name, done, total))
                        }
                    }
                    if (len > 0 && done != total) throw IOException("download cut short")
                }
            }
        } finally {
            conn.disconnect()
        }
        if (!part.renameTo(to)) throw IOException("rename ${part.name}")
    }

    /** Extract the wanted files into `<dir>.tmp`, then move it into place. */
    private fun unpack(b: Archive, archive: File, progress: (Step) -> Unit) {
        val tmp = File(dir, "${b.dir}.tmp")
        tmp.deleteRecursively()
        tmp.mkdirs()
        val size = archive.length().coerceAtLeast(1).toFloat()
        val spec = b.files.entries.joinToString("\n") { "${it.key}\t${it.value}" }
        var error: Throwable? = null
        val worker = thread(name = "vox-unpack") {
            error = runCatching { Native.unpack(archive.path, tmp.path, spec) }.exceptionOrNull()
        }
        // Progress is compressed bytes read, so skipped entries count too.
        while (worker.isAlive) {
            progress(Step.Unpacking(b.dir, (Native.unpackProgress() / size).coerceIn(0f, 1f)))
            worker.join(250)
        }
        error?.let {
            tmp.deleteRecursively()
            archive.delete() // likely corrupt: fetch it again next time
            throw IOException("${b.dir}: ${it.message}", it)
        }
        val missing = b.files.values.filter { !File(tmp, it).isFile }
        if (missing.isNotEmpty()) {
            tmp.deleteRecursively()
            archive.delete()
            throw IOException("${b.dir}: archive lacks ${missing.joinToString()}")
        }
        val final = File(dir, b.dir)
        final.deleteRecursively()
        if (!tmp.renameTo(final)) throw IOException("rename ${tmp.name}")
    }
}
