package com.enigmacurry.rpg_vox_scribe

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/**
 * One model, installed as `<models>/<dir>/`. Two kinds, because the two
 * places these come from publish them differently: [Archive] is a
 * sherpa-onnx release `.tar.bz2` with the wanted files renamed out of
 * it, [Loose] is a Hugging Face repo whose files are downloaded one by
 * one. Same sources and layout as vox_scribe's `download-models`, so a
 * desktop models directory would work as-is.
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

/** What the first run downloads: the two models nothing works without. */
val BUNDLES = listOf(ZIPFORMER_EN, PARAKEET)

/**
 * A language the app can read in or listen out for.
 *
 * [script] is the alphabet it is written in, and it decides everything
 * about which languages can be used together: a paragraph's language is
 * read off its alphabet (`vox_transcribe::language`), so two languages
 * sharing one cannot be told apart and the engine refuses the pair.
 * Greek, Han, Hangul and Kana are one [Script.OTHER] between them for
 * the same reason -- the test is only ever "not the alphabet you read".
 *
 * [code] is how opus-mt spells it, for the translation model's name.
 * [streaming] is the pass-1 bundle for reading in this language, null
 * for the languages that have none, which cannot be the one you read in.
 */
enum class Language(
    val label: String,
    val code: String,
    val script: Script,
    val streaming: Model? = null,
) {
    ENGLISH("English", "en", Script.LATIN, ZIPFORMER_EN),
    RUSSIAN("Russian", "ru", Script.CYRILLIC, ZIPFORMER_RU),
    UKRAINIAN("Ukrainian", "uk", Script.CYRILLIC),
    FRENCH("French", "fr", Script.LATIN),
    SPANISH("Spanish", "es", Script.LATIN),
    CHINESE("Chinese", "zh", Script.OTHER),
    JAPANESE("Japanese", "jap", Script.OTHER),
    KOREAN("Korean", "ko", Script.OTHER);

    companion object {
        /** By the name vox_transcribe spells it with; English if unknown. */
        fun of(label: String?) = entries.firstOrNull { it.label == label } ?: ENGLISH
    }
}

/** The alphabets [Language] is sorted into. */
enum class Script(val label: String) {
    LATIN("Latin"),
    CYRILLIC("Cyrillic"),
    OTHER("another alphabet"),
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
 * exist for some pairs and not others; these are the ones that do, of
 * the languages whose alphabet differs from the native one (the rest
 * could never be told apart anyway).
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

/** Directions with a published model, checked one by one. */
private val TRANSLATIONS = setOf(
    "ru-en", "uk-en", "zh-en", "jap-en", "ko-en",
    "en-ru", "fr-ru", "es-ru", "ko-ru",
)

/** Every language that can be translated into [into]. */
fun translatable(into: Language): List<Language> =
    Language.entries.filter { it != into && translation(it, into) != null }

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
            }
        }
        downloads.deleteRecursively()
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
