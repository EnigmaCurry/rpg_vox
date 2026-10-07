package com.enigmacurry.voxscribe

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/**
 * One sherpa-onnx model bundle: an upstream `.tar.bz2` and the files to
 * keep from it, renamed into `<models>/<dir>/`. Same sources and layout
 * as vox_scribe's `download-models`.
 */
class Bundle(
    val dir: String,
    val stem: String,
    /** Archive path (below the top folder) to installed name. */
    val files: Map<String, String>,
    /** Approximate archive size, for progress before the server says. */
    val approxBytes: Long,
) {
    val url = "$RELEASES/asr-models/$stem.tar.bz2"
}

private const val RELEASES = "https://github.com/k2-fsa/sherpa-onnx/releases/download"

val BUNDLES = listOf(
    Bundle(
        dir = "streaming-zipformer",
        stem = "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17",
        files = mapOf(
            "encoder-epoch-99-avg-1.int8.onnx" to "encoder.onnx",
            "decoder-epoch-99-avg-1.onnx" to "decoder.onnx",
            "joiner-epoch-99-avg-1.int8.onnx" to "joiner.onnx",
            "tokens.txt" to "tokens.txt",
        ),
        approxBytes = 127_887_156,
    ),
    Bundle(
        dir = "parakeet-tdt-0.6b-v2",
        stem = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        files = listOf("encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt")
            .associateWith { it },
        approxBytes = 482_468_385,
    ),
)

sealed interface Step {
    data class Downloading(val bundle: String, val done: Long, val total: Long) : Step
    data class Unpacking(val bundle: String, val fraction: Float) : Step
}

/** App-private model store under `noBackupFilesDir/models`. */
class ModelStore(root: File) {
    val dir = File(root, "models")
    private val downloads = File(root, "downloads")

    fun installed(b: Bundle) = b.files.values.all { File(dir, "${b.dir}/$it").isFile }

    fun ready() = BUNDLES.all(::installed)

    /** Some download or unpack was left part-way. */
    fun started() = downloads.isDirectory && (downloads.list()?.isNotEmpty() == true)

    /** Bytes on disk for installed models. */
    fun size(): Long = dir.walkTopDown().filter { it.isFile }.sumOf { it.length() }

    /**
     * Fetch and unpack every missing bundle. Partial downloads are kept
     * and resumed with an HTTP Range request on the next call.
     */
    fun install(progress: (Step) -> Unit) {
        downloads.mkdirs()
        dir.mkdirs()
        for (b in BUNDLES) {
            if (installed(b)) continue
            val archive = File(downloads, "${b.stem}.tar.bz2")
            if (!archive.isFile) download(b, archive, progress)
            unpack(b, archive, progress)
            archive.delete()
        }
        downloads.deleteRecursively()
    }

    private fun download(b: Bundle, archive: File, progress: (Step) -> Unit) {
        val part = File(downloads, "${b.stem}.tar.bz2.part")
        var have = if (part.isFile) part.length() else 0L
        val conn = (URL(b.url).openConnection() as HttpURLConnection).apply {
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
                    part.renameTo(archive)
                    return
                }
                else -> throw IOException("HTTP $code for ${b.url}")
            }
            val len = conn.getHeaderFieldLong("Content-Length", -1)
            val total = if (len > 0) have + len else b.approxBytes
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
                            progress(Step.Downloading(b.dir, done, total))
                        }
                    }
                    if (len > 0 && done != total) throw IOException("download cut short")
                }
            }
        } finally {
            conn.disconnect()
        }
        if (!part.renameTo(archive)) throw IOException("rename ${part.name}")
    }

    /** Extract the wanted files into `<dir>.tmp`, then move it into place. */
    private fun unpack(b: Bundle, archive: File, progress: (Step) -> Unit) {
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
