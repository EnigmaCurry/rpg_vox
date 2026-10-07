package com.enigmacurry.voxscribe

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

    /** Starts one recording session fed at [sampleRate] Hz mono. */
    external fun start(models: Long, sampleRate: Int): Long
    external fun push(session: Long, samples: FloatArray, len: Int)
    external fun breakParagraph(session: Long)

    /** JSON array of events, waiting up to [timeoutMs] for the first. */
    external fun poll(session: Long, timeoutMs: Int): String

    /** Drains every pass and frees the session; JSON `{"paragraphs": [...]}`. Blocks. */
    external fun finish(session: Long): String
}
