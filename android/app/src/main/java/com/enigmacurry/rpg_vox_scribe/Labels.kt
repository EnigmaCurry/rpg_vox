package com.enigmacurry.rpg_vox_scribe

import android.content.Context

/**
 * Stored words, said in the language being read.
 *
 * A few things the app keeps are English whatever the screen is in,
 * because they are identities rather than text: a paragraph's speaker
 * and the language the engine says it heard are written into the thread
 * database, and a `Language` is handed to the engine by name. Changing
 * the phone's language must not rewrite yesterday's transcript, so it
 * does not: the stored word stays, and these are where it becomes
 * something to read.
 */

/** A paragraph's speaker, stored as "Me" or "Phone"; anything else as it is. */
fun speakerLabel(context: Context, speaker: String): String = when (speaker) {
    SPEAKER_ME -> context.getString(R.string.speaker_me)
    SPEAKER_PHONE -> context.getString(R.string.speaker_phone)
    else -> speaker
}

/**
 * The language the engine says a paragraph was heard in. It names
 * twenty-five of them and the app knows eight, so one it does not know
 * is shown as the engine spelled it -- English, but the alternative is
 * showing nothing at all.
 */
fun languageName(context: Context, engineName: String): String =
    Language.entries.firstOrNull { it.label == engineName }
        ?.let { context.getString(it.nameRes) }
        ?: engineName

/** As stored with a paragraph, and as `Para.source` carries it. */
const val SPEAKER_ME = "Me"
const val SPEAKER_PHONE = "Phone"
