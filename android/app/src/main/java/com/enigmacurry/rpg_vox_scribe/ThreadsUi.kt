package com.enigmacurry.rpg_vox_scribe

import androidx.annotation.StringRes
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.ImeAction
import kotlinx.coroutines.delay
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt

/** Rename or delete requests raised from the drawer or the title, shown as dialogs. */
class ThreadDialogs {
    var renaming by mutableStateOf<ThreadInfo?>(null)
    var deleting by mutableStateOf<ThreadInfo?>(null)
    var renamingDictation by mutableStateOf<DictationInfo?>(null)
    var deletingDictation by mutableStateOf<DictationInfo?>(null)
    /** New was tapped: transcription or dictation? */
    var choosingNew by mutableStateOf(false)
}

/** A transcription (thread) and a dictation, as the drawer marks them. */
private const val MIC = "🎙️"
private const val PEN = "✍️"

/**
 * The drawer: New, which asks whether for a transcription or a
 * dictation, then both kinds in one list, the most recently changed
 * first, each with its time and last words and marked by its icon. Tap
 * opens one; long-press offers Rename and Delete. While recording only
 * the open transcription is reachable.
 */
@Composable
fun ThreadDrawer(dialogs: ThreadDialogs, close: () -> Unit) {
    val locked = Scribe.busy()
    val dictation = Dictations.open
    // Both kinds' times are local "YYYY-MM-DD HH:MM", so they sort as text.
    val entries = (Scribe.threadList.map { it.last to it } + Dictations.list.map { it.updated to it })
        .sortedByDescending { it.first }
        .map { it.second }
    ModalDrawerSheet {
        Text(
            stringResource(R.string.app_name),
            style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.padding(start = 24.dp, top = 24.dp, bottom = 8.dp),
        )
        if (locked) {
            Text(
                stringResource(R.string.drawer_locked),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(start = 24.dp, bottom = 8.dp),
            )
        }
        ThreadRow(
            icon = null,
            title = stringResource(R.string.drawer_new),
            subtitle = null,
            selected = false,
            enabled = !locked,
            onClick = {
                dialogs.choosingNew = true
                close()
            },
        )
        HorizontalDivider(Modifier.padding(vertical = 8.dp))
        LazyColumn {
            items(entries, key = { if (it is DictationInfo) "d${it.id}" else "t${(it as ThreadInfo).id}" }) { e ->
                var menu by remember { mutableStateOf(false) }
                Box {
                    when (e) {
                        is DictationInfo -> ThreadRow(
                            icon = PEN,
                            title = e.title,
                            subtitle = e.updated + e.text.take(80).replace('\n', ' ').let { if (it.isNotBlank()) " · $it" else "" },
                            selected = e.id == dictation?.id,
                            enabled = !locked,
                            onClick = {
                                Dictations.openDictation(e.id)
                                close()
                            },
                            onLongClick = { menu = true },
                        )
                        is ThreadInfo -> {
                            val open = e.id == Scribe.thread.id
                            ThreadRow(
                                icon = MIC,
                                title = e.title,
                                subtitle = e.last + if (e.preview.isNotBlank()) " · ${e.preview}" else "",
                                selected = open && dictation == null,
                                enabled = open || !locked,
                                onClick = {
                                    Dictations.close()
                                    Scribe.switchTo(e.id)
                                    close()
                                },
                                onLongClick = { menu = true },
                            )
                        }
                    }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text(stringResource(R.string.action_rename)) }, onClick = {
                            menu = false
                            if (e is DictationInfo) dialogs.renamingDictation = e else dialogs.renaming = e as ThreadInfo
                        })
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.action_delete)) },
                            enabled = !(e is ThreadInfo && e.id == Scribe.thread.id && locked),
                            onClick = {
                                menu = false
                                if (e is DictationInfo) dialogs.deletingDictation = e else dialogs.deleting = e as ThreadInfo
                            },
                        )
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ThreadRow(
    icon: String?,
    title: String,
    subtitle: String?,
    selected: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
    onLongClick: (() -> Unit)? = null,
) {
    Surface(
        color = if (selected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(28.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 2.dp)
            .alpha(if (enabled) 1f else 0.4f)
            // Long-press works even when tapping is locked (rename while recording).
            .combinedClickable(onClick = { if (enabled) onClick() }, onLongClick = onLongClick),
    ) {
        androidx.compose.foundation.layout.Row(
            Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        ) {
            if (icon != null) Text(icon, style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(end = 12.dp))
        Column {
            Text(
                title,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = if (selected) FontWeight.Bold else FontWeight.Normal,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (subtitle != null) {
                Text(
                    subtitle,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        }
    }
}

@Composable
fun ThreadDialogHost(dialogs: ThreadDialogs) {
    if (dialogs.choosingNew) {
        val done = { dialogs.choosingNew = false }
        AlertDialog(
            onDismissRequest = done,
            title = { Text(stringResource(R.string.new_thread_title)) },
            text = {
                Column {
                    NewChoice(
                        PEN,
                        stringResource(R.string.new_dictation),
                        stringResource(R.string.new_dictation_detail),
                    ) {
                        done()
                        Dictations.create()
                        if (Scribe.autoStart) Scribe.startWhenOpen = "dictation"
                    }
                    NewChoice(
                        MIC,
                        stringResource(R.string.new_transcription),
                        stringResource(R.string.new_transcription_detail),
                    ) {
                        done()
                        Dictations.close()
                        Scribe.newThread()
                        if (Scribe.autoStart) Scribe.startWhenOpen = "transcription"
                    }
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = done) { Text(stringResource(R.string.action_cancel)) } },
        )
    }
    dialogs.renaming?.let { t ->
        Rename(
            stringResource(R.string.rename_transcription),
            t.title,
            close = { dialogs.renaming = null },
        ) { Scribe.rename(t.id, it) }
    }
    dialogs.renamingDictation?.let { d ->
        Rename(
            stringResource(R.string.rename_dictation),
            d.title,
            close = { dialogs.renamingDictation = null },
        ) { Dictations.rename(d.id, it) }
    }
    dialogs.deleting?.let { t ->
        ConfirmDelete(
            stringResource(R.string.delete_transcription_title),
            pluralStringResource(R.plurals.delete_transcription_body, t.count, t.title, t.count),
            close = { dialogs.deleting = null },
        ) { Scribe.delete(t.id) }
    }
    dialogs.deletingDictation?.let { d ->
        ConfirmDelete(
            stringResource(R.string.delete_dictation_title),
            stringResource(R.string.delete_dictation_body, d.title),
            close = { dialogs.deletingDictation = null },
        ) { Dictations.delete(d.id) }
    }
}

@Composable
private fun NewChoice(icon: String, title: String, detail: String, onClick: () -> Unit) {
    Surface(
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        onClick = onClick,
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
    ) {
        androidx.compose.foundation.layout.Row(
            Modifier.padding(16.dp),
            verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        ) {
            Text(icon, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.padding(end = 16.dp))
            Column {
                Text(title, style = MaterialTheme.typography.titleMedium)
                Text(detail, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

/** Asks for a new name for [current]; [rename] takes it. */
@Composable
private fun Rename(heading: String, current: String, close: () -> Unit, rename: (String) -> Unit) {
    // Whole title selected, so typing replaces it.
    var text by remember(current) { mutableStateOf(TextFieldValue(current, TextRange(0, current.length))) }
    AlertDialog(
        onDismissRequest = close,
        title = { Text(heading) },
        text = {
            // Focused with the keyboard up, ready to type; Done renames.
            val focus = remember { FocusRequester() }
            val keyboard = LocalSoftwareKeyboardController.current
            LaunchedEffect(Unit) {
                // The dialog's window must be up before it can take focus.
                delay(100)
                focus.requestFocus()
                keyboard?.show()
            }
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = {
                    if (text.text.isNotBlank()) {
                        rename(text.text)
                        close()
                    }
                }),
                modifier = Modifier.focusRequester(focus),
            )
        },
        confirmButton = {
            TextButton(
                onClick = {
                    rename(text.text)
                    close()
                },
                enabled = text.text.isNotBlank(),
            ) { Text(stringResource(R.string.action_rename)) }
        },
        dismissButton = { TextButton(onClick = close) { Text(stringResource(R.string.action_cancel)) } },
    )
}

@Composable
private fun ConfirmDelete(heading: String, detail: String, close: () -> Unit, delete: () -> Unit) {
    AlertDialog(
        onDismissRequest = close,
        title = { Text(heading) },
        text = { Text(detail) },
        confirmButton = {
            TextButton(onClick = {
                delete()
                close()
            }) { Text(stringResource(R.string.action_delete), color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = close) { Text(stringResource(R.string.action_cancel)) } },
    )
}

/** The pages of settings, listed under ⚙. */
enum class SettingsPage(@StringRes val titleRes: Int) {
    MICROPHONE(R.string.settings_microphone),
    RECORDING(R.string.settings_recording),
    LANGUAGES(R.string.settings_languages),
    KEYBOARD(R.string.settings_keyboard),
    DISPLAY(R.string.settings_display),
    ABOUT(R.string.settings_about),
}

/** One settings page as a dialog. */
@Composable
fun Settings(page: SettingsPage, close: () -> Unit) {
    if (page == SettingsPage.ABOUT) {
        About(close)
        return
    }
    AlertDialog(
        onDismissRequest = close,
        title = { Text(stringResource(page.titleRes)) },
        text = {
            Column(Modifier.verticalScroll(androidx.compose.foundation.rememberScrollState())) {
                when (page) {
                    SettingsPage.MICROPHONE -> MicrophonePage()
                    SettingsPage.RECORDING -> RecordingPage()
                    SettingsPage.LANGUAGES -> LanguagesPage()
                    SettingsPage.KEYBOARD -> KeyboardPage()
                    SettingsPage.DISPLAY -> DisplayPage()
                    SettingsPage.ABOUT -> {}
                }
            }
        },
        confirmButton = { TextButton(onClick = close) { Text(stringResource(R.string.action_done)) } },
    )
}

/** Which mic, and its gain. Take effect when the mic next starts. */
@Composable
private fun MicrophonePage() {
    Text(stringResource(R.string.mic_which), style = MaterialTheme.typography.titleSmall)
    MicRoute.entries.forEach { r ->
        Choice(stringResource(r.labelRes), Scribe.route == r) { Scribe.chooseRoute(r) }
    }
    Text(
        stringResource(R.string.mic_boost),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(top = 12.dp),
    )
    Boost.entries.forEach { b ->
        // Auto says what it does here, where there is room for it.
        val label = stringResource(if (b == Boost.AUTO) R.string.boost_auto_long else b.labelRes)
        Choice(label, Scribe.boost == b) { Scribe.chooseBoost(b) }
    }
    if (Scribe.recording) {
        Text(
            stringResource(R.string.mic_applies_next_start),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 8.dp),
        )
    }
}

/** What's kept of a recording. Takes effect from the next recording. */
@Composable
private fun RecordingPage() {
    Toggle(
        stringResource(R.string.keep_audio),
        stringResource(R.string.keep_audio_detail),
        Scribe.keepAudio,
        Scribe::chooseKeepAudio,
    )
    Toggle(
        stringResource(R.string.keep_non_speech),
        stringResource(R.string.keep_non_speech_detail),
        Scribe.keepNonSpeech,
        Scribe::chooseKeepNonSpeech,
        enabled = Scribe.keepAudio,
    )
    Toggle(
        stringResource(R.string.auto_start),
        stringResource(R.string.auto_start_detail),
        Scribe.autoStart,
        Scribe::chooseAutoStart,
    )
}

@Composable
private fun KeyboardPage() {
    val context = androidx.compose.ui.platform.LocalContext.current
    Text(stringResource(R.string.keyboard_how), style = MaterialTheme.typography.bodyMedium)
    TextButton(onClick = {
        context.startActivity(
            android.content.Intent(android.provider.Settings.ACTION_INPUT_METHOD_SETTINGS)
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }) { Text(stringResource(R.string.keyboard_settings)) }
}

/**
 * The language you read in, the secondary ones to listen out for, the
 * dictionaries they need, and what a paragraph shows while its reading
 * catches up.
 *
 * Each other language is one model, downloaded here, that reads it in
 * the language you read. Which languages can be offered is not a matter
 * of taste: a paragraph's language is read off its alphabet, so one
 * sharing the alphabet you read in could never be told apart from your
 * own, and two sharing each other's could not be told apart either. So
 * the list is the languages with a model into yours, and turning one on
 * turns off whichever other one shares its alphabet.
 *
 * All of which is said here in a line apiece. A settings page that
 * explains itself in paragraphs is one nobody reads to the end of, and
 * everything it would have said is in ANDROID.md, where there is room
 * for it.
 */
@Composable
private fun LanguagesPage() {
    val installing = InstallService.state.collectAsState().value
    // Recomposes when a model is installed or deleted.
    val models = Scribe.languageModels
    val native = Scribe.native
    Text(stringResource(R.string.languages_i_read_in), style = MaterialTheme.typography.titleSmall)
    NATIVE_LANGUAGES.forEach { l ->
        Choice(stringResource(l.nameRes), native == l) { Scribe.chooseNative(l) }
    }
    Hint(stringResource(R.string.languages_native_hint, stringResource(native.nameRes)))
    Scribe.pass1Missing?.let { l ->
        Hint(stringResource(R.string.languages_pass1_missing, stringResource(l.nameRes)), error = true)
    }

    Text(
        stringResource(R.string.languages_secondary),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(top = 16.dp),
    )
    val chosen = Scribe.others
    translatable(native).forEach { l ->
        val model = translation(l, native) ?: return@forEach
        val on = l in chosen
        // The language already chosen that this one could not be told
        // apart from, if any: turning it on would replace that one.
        val clash = if (on) null else chosen.firstOrNull { it.script == l.script }
        val have = remember(models) { Scribe.store.installed(model) }
        val mb = model.approxBytes / 1_000_000
        Toggle(
            stringResource(l.nameRes),
            when {
                clash != null -> stringResource(
                    R.string.languages_clash,
                    stringResource(l.script.labelRes),
                    stringResource(clash.nameRes),
                )
                have -> stringResource(R.string.languages_have, mb)
                on -> stringResource(R.string.languages_to_download, mb)
                else -> stringResource(R.string.languages_size, mb)
            },
            on,
            { Scribe.chooseOther(l, it) },
            enabled = clash == null && !Scribe.busy(),
        )
    }
    if (translatable(native).isEmpty()) {
        Hint(stringResource(R.string.languages_none, stringResource(native.nameRes)))
    }

    // What the choices above still want fetched, and what they left behind.
    val missing = remember(models, chosen, native) { Scribe.wanted().filterNot(Scribe.store::installed) }
    val spare = remember(models, chosen, native) { Scribe.spare() }
    // Only when there is one to fetch or one to be rid of: a header
    // over nothing is the sort of thing this page is being rid of.
    if (missing.isNotEmpty() || spare.isNotEmpty() || installing !is Install.Idle) {
        Text(
            stringResource(R.string.languages_dictionaries),
            style = MaterialTheme.typography.titleSmall,
            modifier = Modifier.padding(top = 16.dp),
        )
    }
    val context = androidx.compose.ui.platform.LocalContext.current
    when {
        installing is Install.Running -> Hint(describe(context, installing.step))
        installing is Install.Failed ->
            Hint(stringResource(R.string.download_failed, installing.error), error = true)
        missing.isNotEmpty() ->
            Hint(
                stringResource(
                    R.string.languages_fetch_hint,
                    missing.sumOf { it.approxBytes } / 1_000_000,
                ),
            )
    }
    if (missing.isNotEmpty() && installing !is Install.Running) {
        TextButton(onClick = Scribe::installLanguages) {
            Text(stringResource(if (installing is Install.Failed) R.string.action_retry else R.string.action_download))
        }
    }
    spare.forEach { m ->
        TextButton(onClick = { Scribe.removeModel(m) }) {
            Text(stringResource(R.string.languages_delete_model, modelName(context, m)))
        }
    }

    Text(
        stringResource(R.string.languages_while_catching_up),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(top = 16.dp),
    )
    Toggle(
        stringResource(R.string.languages_show_said),
        stringResource(R.string.languages_show_said_detail),
        Scribe.showSaid,
        Scribe::chooseShowSaid,
    )

    if (Scribe.recording || Scribe.busy()) {
        Hint(stringResource(R.string.languages_next_recording))
    }
}

/**
 * A model left on disk by a language no longer chosen, said in
 * languages rather than in the directory it is kept in: "ru-en" is the
 * app's business and "Russian → English" is the reader's. Its size is
 * not repeated here -- the list above says what one of these costs,
 * and a button that wraps onto two lines to say it again is the kind
 * of thing this page is being rid of.
 */
private fun modelName(context: android.content.Context, m: Model): String {
    val pair = m.dir.removePrefix("translate/").split('-')
    val from = Language.entries.firstOrNull { it.code == pair.firstOrNull() }
    val into = Language.entries.firstOrNull { it.code == pair.lastOrNull() }
    return when {
        pair.size == 2 && from != null && into != null -> context.getString(
            R.string.model_pair,
            context.getString(from.nameRes),
            context.getString(into.nameRes),
        )
        else -> Language.entries.firstOrNull { it.streaming?.dir == m.dir }
            ?.let { context.getString(R.string.model_live_text, context.getString(it.nameRes)) }
            ?: m.dir.substringAfterLast('/')
    }
}

/** A line of explanation under a choice, or a warning in red. */
@Composable
private fun Hint(text: String, error: Boolean = false) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = if (error) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 4.dp),
    )
}

/** Transcript text size: five steps, the default in the middle. */
@Composable
private fun DisplayPage() {
    Text(stringResource(R.string.display_font_size), style = MaterialTheme.typography.titleSmall)
    androidx.compose.material3.Slider(
        value = Scribe.fontStep.toFloat(),
        onValueChange = { Scribe.chooseFontStep(it.roundToInt()) },
        valueRange = 0f..(FONT_SCALES.size - 1).toFloat(),
        steps = FONT_SCALES.size - 2,
    )
    Text(
        stringResource(R.string.display_sample),
        style = MaterialTheme.typography.bodyLarge.scaled(FONT_SCALES[Scribe.fontStep]),
    )
}


@Composable
private fun Toggle(
    title: String,
    detail: String,
    value: Boolean,
    onChange: (Boolean) -> Unit,
    enabled: Boolean = true,
) {
    androidx.compose.foundation.layout.Row(
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 12.dp)
            .alpha(if (enabled) 1f else 0.4f)
            .toggleable(value = value, enabled = enabled, role = Role.Switch, onValueChange = onChange),
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(detail, style = MaterialTheme.typography.bodySmall)
        }
        androidx.compose.material3.Switch(checked = value, onCheckedChange = null, enabled = enabled)
    }
}

@Composable
private fun Choice(label: String, selected: Boolean, onClick: () -> Unit) {
    androidx.compose.foundation.layout.Row(
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .selectable(selected = selected, onClick = onClick, role = Role.RadioButton),
    ) {
        RadioButton(selected = selected, onClick = null)
        Text(label, modifier = Modifier.padding(start = 8.dp))
    }
}
