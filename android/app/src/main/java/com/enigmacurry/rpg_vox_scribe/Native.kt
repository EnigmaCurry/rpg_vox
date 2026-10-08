package com.enigmacurry.rpg_vox_scribe

/** JNI bridge to crates/vox_android. Handles are opaque pointers. */
object Native {
    init {
        System.loadLibrary("onnxruntime")
        System.loadLibrary("sherpa-onnx-c-api")
        System.loadLibrary("vox_android")
    }

    /**
     * Extracts entries from a `.tar.bz2` into [destDir]; [spec] has one
     * `<path below the top folder>\t<installed name>` per line. Blocks; throws on failure.
     */
    external fun unpack(archive: String, destDir: String, spec: String)

    /** Compressed bytes the running [unpack] has read. */
    external fun unpackProgress(): Long

    /** Loads Parakeet + Zipformer from [modelsDir]; throws on failure. Slow (seconds). */
    external fun load(modelsDir: String, threads: Int): Long
    external fun freeModels(models: Long)

    /**
     * Starts one session (one source) fed at [sampleRate] Hz mono; [streaming]
     * adds live partials. With [opusPath] the pushed audio is also kept there
     * as Ogg Opus, closed by [finish]; throws if it can't be created.
     */
    external fun start(models: Long, sampleRate: Int, streaming: Boolean, opusPath: String?): Long
    external fun push(session: Long, samples: FloatArray, len: Int)
    external fun breakParagraph(session: Long)

    /** JSON array of events, waiting up to [timeoutMs] for the first. */
    external fun poll(session: Long, timeoutMs: Int): String

    /**
     * Drains every pass; JSON `{"paragraphs": [...]}`. Blocks. [poll] and
     * [push] stay safe meanwhile (pushes are dropped); call [free] after.
     */
    external fun finish(session: Long): String
    external fun free(session: Long)

    /** Opens an Ogg Opus recording for playback, at its start; throws on failure. */
    external fun playerOpen(path: String): Long
    /** Length of the recording in ms. */
    external fun playerLength(player: Long): Long
    external fun playerSeek(player: Long, ms: Long)
    /** Fills [out] with mono float samples at 48 kHz; how many, fewer only at the end. */
    external fun playerRead(player: Long, out: FloatArray): Int
    external fun playerFree(player: Long)
}
