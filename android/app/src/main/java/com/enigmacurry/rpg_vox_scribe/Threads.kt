package com.enigmacurry.rpg_vox_scribe

import android.content.ContentValues
import android.database.sqlite.SQLiteDatabase
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** A thread in the list: [last] is local "YYYY-MM-DD HH:MM" of its latest text (or creation). */
data class ThreadInfo(val id: Long, val title: String, val last: String, val preview: String, val count: Int)

/**
 * One stored paragraph. [audio] is the recording it was heard in and
 * [words] its word timings in that recording (JSON, see [Word.toJson]).
 * [atMs] is when it was said (epoch ms), which orders a thread.
 */
data class Saved(
    val rowId: Long,
    val text: String,
    val speaker: String?,
    val audio: String? = null,
    val words: String? = null,
    val atMs: Long = 0,
)

private val iso = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", Locale.ROOT).apply {
    timeZone = java.util.TimeZone.getTimeZone("UTC")
}

/** The desktop's `at` format, from epoch ms. */
private fun isoAt(ms: Long): String = synchronized(iso) { iso.format(Date(ms)) }

private fun parseAt(at: String?): Long =
    at?.let { runCatching { synchronized(iso) { iso.parse(it)?.time } }.getOrNull() } ?: 0L

/**
 * A kept file placed on a timeline: it starts at [offsetMs] (its time
 * zero; negative when its head is cut off) and plays until [untilMs].
 */
data class AudioFile(val file: String, val offsetMs: Long, val untilMs: Long = Long.MAX_VALUE)

/**
 * One source's file of [recording], [offsetMs] into it and [lengthMs]
 * long (0: not known yet), with sound from [soundStartMs] to
 * [soundEndMs] (-1: not looked for yet).
 */
data class AudioRow(
    val file: String,
    val recording: String,
    val offsetMs: Long,
    val lengthMs: Long,
    val soundStartMs: Long,
    val soundEndMs: Long,
)

/**
 * Threads (named transcripts) in `files/scribe.db`, using the desktop
 * chat database's schema (crates/vox_chat): `conversations` are threads,
 * each finished paragraph is a `messages` row (`role='user'`,
 * `kind='say'`), and `settings.conversation` is the thread last open.
 * `messages.at` is when a paragraph was said rather than stored, so a
 * thread lists in speaking order even when a paragraph was finished after
 * a later one. Additions, ignored by the desktop: `messages.speaker` ("Me",
 * "Phone") for recordings with two sources, and for kept audio
 * `messages.audio` and `messages.words` (see [Saved]) plus the `audio`
 * table, which groups the files a recording made (one per source) with
 * each one's start in it, so they play back together.
 */
class ThreadStore(file: File) {
    private val db: SQLiteDatabase = SQLiteDatabase.openOrCreateDatabase(file, null).apply {
        enableWriteAheadLogging()
        execSQL(
            """CREATE TABLE IF NOT EXISTS conversations (
                id         INTEGER PRIMARY KEY,
                title      TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            )""",
        )
        execSQL(
            """CREATE TABLE IF NOT EXISTS messages (
                id       INTEGER PRIMARY KEY,
                conversation INTEGER REFERENCES conversations(id),
                role     TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
                kind     TEXT    NOT NULL DEFAULT 'say' CHECK (kind IN ('say', 'interrupt')),
                reply_to INTEGER REFERENCES messages(id),
                text     TEXT    NOT NULL DEFAULT '',
                more     INTEGER NOT NULL DEFAULT 0,
                at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            )""",
        )
        execSQL("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        for (column in listOf("speaker", "audio", "words")) {
            rawQuery("SELECT 1 FROM pragma_table_info('messages') WHERE name = ?", arrayOf(column)).use {
                if (!it.moveToFirst()) execSQL("ALTER TABLE messages ADD COLUMN $column TEXT")
            }
        }
        execSQL(
            """CREATE TABLE IF NOT EXISTS audio (
                file         TEXT PRIMARY KEY,
                conversation INTEGER REFERENCES conversations(id),
                recording    TEXT NOT NULL,
                offset_ms    INTEGER NOT NULL DEFAULT 0
            )""",
        )
        // Stretches of a kept file whose text was cut: skipped on playback.
        execSQL("CREATE TABLE IF NOT EXISTS cuts (file TEXT NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL)")
        for ((column, default) in listOf("length_ms" to 0, "sound_start_ms" to -1, "sound_end_ms" to -1)) {
            rawQuery("SELECT 1 FROM pragma_table_info('audio') WHERE name = ?", arrayOf(column)).use {
                if (!it.moveToFirst()) execSQL("ALTER TABLE audio ADD COLUMN $column INTEGER NOT NULL DEFAULT $default")
            }
        }
    }

    /** A new thread titled [title], or with the local date and time like the desktop's. */
    fun create(title: String? = null): Long {
        val t = title ?: SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.ROOT).format(Date())
        return db.insert("conversations", null, ContentValues().apply { put("title", t) })
    }

    fun rename(id: Long, title: String) {
        db.update("conversations", ContentValues().apply { put("title", title) }, "id = ?", arrayOf("$id"))
    }

    /** Delete thread [id]; its audio files, for the caller to remove. */
    fun delete(id: Long): List<String> {
        val files = audioFiles(id)
        db.beginTransaction()
        try {
            files.forEach { db.delete("cuts", "file = ?", arrayOf(it)) }
            db.delete("messages", "conversation = ?", arrayOf("$id"))
            db.delete("audio", "conversation = ?", arrayOf("$id"))
            db.delete("conversations", "id = ?", arrayOf("$id"))
            db.setTransactionSuccessful()
        } finally {
            db.endTransaction()
        }
        return files
    }

    fun exists(id: Long) =
        db.rawQuery("SELECT 1 FROM conversations WHERE id = ?", arrayOf("$id")).use { it.moveToFirst() }

    /** Every thread, the most recently active first (as the desktop lists them). */
    fun list(): List<ThreadInfo> =
        db.rawQuery(
            """SELECT c.id, c.title,
                      strftime('%Y-%m-%d %H:%M', coalesce(max(m.at), c.created_at), 'localtime'),
                      (SELECT text FROM messages WHERE conversation = c.id ORDER BY id DESC LIMIT 1),
                      count(m.id)
               FROM conversations c LEFT JOIN messages m ON m.conversation = c.id
               GROUP BY c.id
               ORDER BY coalesce(max(m.at), c.created_at) DESC, c.id DESC""",
            null,
        ).use { c ->
            buildList {
                while (c.moveToNext()) {
                    add(ThreadInfo(c.getLong(0), c.getString(1), c.getString(2), c.getString(3) ?: "", c.getInt(4)))
                }
            }
        }

    /** The thread last open, else the most recent, else a new one. */
    fun current(): Long {
        val saved = setting("conversation")?.toLongOrNull()
        if (saved != null && exists(saved)) return saved
        return list().firstOrNull()?.id ?: create()
    }

    fun setCurrent(id: Long) = setSetting("conversation", "$id")

    fun messages(thread: Long): List<Saved> =
        db.rawQuery(
            "SELECT id, text, speaker, audio, words, at FROM messages WHERE conversation = ? AND role = 'user' AND kind = 'say' ORDER BY at, id",
            arrayOf("$thread"),
        ).use { c ->
            buildList {
                while (c.moveToNext()) {
                    add(Saved(c.getLong(0), c.getString(1), c.getString(2), c.getString(3), c.getString(4), parseAt(c.getString(5))))
                }
            }
        }

    fun say(
        thread: Long,
        text: String,
        speaker: String?,
        audio: String? = null,
        words: String? = null,
        atMs: Long = 0,
    ): Long =
        db.insert("messages", null, ContentValues().apply {
            if (atMs > 0) put("at", isoAt(atMs))
            put("conversation", thread)
            put("role", "user")
            put("kind", "say")
            put("text", text)
            put("speaker", speaker)
            put("audio", audio)
            put("words", words)
        })

    fun edit(rowId: Long, text: String, words: String? = null, atMs: Long = 0) {
        db.update(
            "messages",
            ContentValues().apply {
                put("text", text)
                put("words", words)
                if (atMs > 0) put("at", isoAt(atMs))
            },
            "id = ?",
            arrayOf("$rowId"),
        )
    }

    /** Register [file], one source of [recording] in [thread]. */
    fun addAudio(thread: Long, recording: String, file: String) {
        db.insertWithOnConflict(
            "audio", null,
            ContentValues().apply {
                put("file", file)
                put("conversation", thread)
                put("recording", recording)
            },
            SQLiteDatabase.CONFLICT_IGNORE,
        )
    }

    /** Where [file] starts in its recording. */
    fun setAudioOffset(file: String, offsetMs: Long) {
        db.update("audio", ContentValues().apply { put("offset_ms", offsetMs) }, "file = ?", arrayOf(file))
    }

    fun setAudioLength(file: String, lengthMs: Long) {
        db.update("audio", ContentValues().apply { put("length_ms", lengthMs) }, "file = ?", arrayOf(file))
    }

    fun removeAudio(file: String) {
        db.delete("audio", "file = ?", arrayOf(file))
        db.delete("cuts", "file = ?", arrayOf(file))
    }

    /** Skip [startMs]..[endMs] of [file] on playback. */
    fun addCut(file: String, startMs: Long, endMs: Long) {
        db.insert("cuts", null, ContentValues().apply {
            put("file", file)
            put("start_ms", startMs)
            put("end_ms", endMs)
        })
    }

    /** [thread]'s cuts, by file. */
    fun cuts(thread: Long): Map<String, List<LongRange>> =
        db.rawQuery(
            "SELECT c.file, c.start_ms, c.end_ms FROM cuts c JOIN audio a ON a.file = c.file WHERE a.conversation = ?",
            arrayOf("$thread"),
        ).use { c ->
            val out = HashMap<String, MutableList<LongRange>>()
            while (c.moveToNext()) out.getOrPut(c.getString(0)) { mutableListOf() } += c.getLong(1)..c.getLong(2)
            out
        }

    fun setAudioSound(file: String, startMs: Long, endMs: Long) {
        db.update(
            "audio",
            ContentValues().apply {
                put("sound_start_ms", startMs)
                put("sound_end_ms", endMs)
            },
            "file = ?",
            arrayOf(file),
        )
    }

    /** [thread]'s kept audio, oldest recording first. */
    fun audioRows(thread: Long): List<AudioRow> =
        db.rawQuery(
            """SELECT file, recording, offset_ms, length_ms, sound_start_ms, sound_end_ms
               FROM audio WHERE conversation = ? ORDER BY recording, file""",
            arrayOf("$thread"),
        ).use { c ->
            buildList {
                while (c.moveToNext()) {
                    add(AudioRow(c.getString(0), c.getString(1), c.getLong(2), c.getLong(3), c.getLong(4), c.getLong(5)))
                }
            }
        }

    private fun audioFiles(thread: Long): List<String> =
        db.rawQuery("SELECT file FROM audio WHERE conversation = ?", arrayOf("$thread")).use { c ->
            buildList { while (c.moveToNext()) add(c.getString(0)) }
        }

    fun remove(rowId: Long) {
        db.delete("messages", "id = ?", arrayOf("$rowId"))
    }

    /** Empty [thread]; its audio files, for the caller to remove. */
    fun clear(thread: Long): List<String> {
        val files = audioFiles(thread)
        files.forEach { db.delete("cuts", "file = ?", arrayOf(it)) }
        db.delete("messages", "conversation = ?", arrayOf("$thread"))
        db.delete("audio", "conversation = ?", arrayOf("$thread"))
        return files
    }

    fun setting(key: String): String? =
        db.rawQuery("SELECT value FROM settings WHERE key = ?", arrayOf(key)).use {
            if (it.moveToFirst()) it.getString(0) else null
        }

    fun setSetting(key: String, value: String) {
        db.insertWithOnConflict(
            "settings", null,
            ContentValues().apply { put("key", key); put("value", value) },
            SQLiteDatabase.CONFLICT_REPLACE,
        )
    }
}
