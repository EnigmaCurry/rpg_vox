package com.enigmacurry.rpg_vox_scribe

import android.content.ContentValues
import android.database.sqlite.SQLiteDatabase
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** A thread in the list: [last] is local "YYYY-MM-DD HH:MM" of its latest text (or creation). */
data class ThreadInfo(val id: Long, val title: String, val last: String, val preview: String, val count: Int)

/** One stored paragraph. */
data class Saved(val rowId: Long, val text: String, val speaker: String?)

/**
 * Threads (named transcripts) in `files/scribe.db`, using the desktop
 * chat database's schema (crates/vox_chat): `conversations` are threads,
 * each finished paragraph is a `messages` row (`role='user'`,
 * `kind='say'`), and `settings.conversation` is the thread last open.
 * One addition, ignored by the desktop: `messages.speaker` ("Me",
 * "Phone") for recordings with two sources.
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
        rawQuery("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'speaker'", null).use {
            if (!it.moveToFirst()) execSQL("ALTER TABLE messages ADD COLUMN speaker TEXT")
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

    fun delete(id: Long) {
        db.beginTransaction()
        try {
            db.delete("messages", "conversation = ?", arrayOf("$id"))
            db.delete("conversations", "id = ?", arrayOf("$id"))
            db.setTransactionSuccessful()
        } finally {
            db.endTransaction()
        }
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
            "SELECT id, text, speaker FROM messages WHERE conversation = ? AND role = 'user' AND kind = 'say' ORDER BY id",
            arrayOf("$thread"),
        ).use { c ->
            buildList { while (c.moveToNext()) add(Saved(c.getLong(0), c.getString(1), c.getString(2))) }
        }

    fun say(thread: Long, text: String, speaker: String?): Long =
        db.insert("messages", null, ContentValues().apply {
            put("conversation", thread)
            put("role", "user")
            put("kind", "say")
            put("text", text)
            put("speaker", speaker)
        })

    fun edit(rowId: Long, text: String) {
        db.update("messages", ContentValues().apply { put("text", text) }, "id = ?", arrayOf("$rowId"))
    }

    fun remove(rowId: Long) {
        db.delete("messages", "id = ?", arrayOf("$rowId"))
    }

    fun clear(thread: Long) {
        db.delete("messages", "conversation = ?", arrayOf("$thread"))
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
