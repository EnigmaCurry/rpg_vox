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

/** A kept recording: one source's audio file and where it starts in its recording. */
data class AudioFile(val file: String, val offsetMs: Long)

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

    /** Register [file], one source of [recording] in [thread], starting [offsetMs] into it. */
    fun addAudio(thread: Long, recording: String, file: String, offsetMs: Long) {
        db.insertWithOnConflict(
            "audio", null,
            ContentValues().apply {
                put("file", file)
                put("conversation", thread)
                put("recording", recording)
                put("offset_ms", offsetMs)
            },
            SQLiteDatabase.CONFLICT_REPLACE,
        )
    }

    /** Every file of the recording [file] belongs to (itself included). */
    fun recording(file: String): List<AudioFile> =
        db.rawQuery(
            "SELECT file, offset_ms FROM audio WHERE recording = (SELECT recording FROM audio WHERE file = ?) ORDER BY file",
            arrayOf(file),
        ).use { c -> buildList { while (c.moveToNext()) add(AudioFile(c.getString(0), c.getLong(1))) } }

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
