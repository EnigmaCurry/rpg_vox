package com.enigmacurry.rpg_vox_scribe

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
            "Scribe",
            style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.padding(start = 24.dp, top = 24.dp, bottom = 8.dp),
        )
        if (locked) {
            Text(
                "Stop recording to switch",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(start = 24.dp, bottom = 8.dp),
            )
        }
        ThreadRow(
            icon = null,
            title = "+ New",
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
                        DropdownMenuItem(text = { Text("Rename") }, onClick = {
                            menu = false
                            if (e is DictationInfo) dialogs.renamingDictation = e else dialogs.renaming = e as ThreadInfo
                        })
                        DropdownMenuItem(
                            text = { Text("Delete") },
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
            title = { Text("New thread") },
            text = {
                Column {
                    NewChoice(PEN, "Dictation", "Write by voice, and keep editing anywhere in the text.") {
                        done()
                        Dictations.create()
                        if (Scribe.autoStart) Scribe.startWhenOpen = "dictation"
                    }
                    NewChoice(MIC, "Transcription", "Record and transcribe in order: talks, meetings, videos.") {
                        done()
                        Dictations.close()
                        Scribe.newThread()
                        if (Scribe.autoStart) Scribe.startWhenOpen = "transcription"
                    }
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = done) { Text("Cancel") } },
        )
    }
    dialogs.renaming?.let { t ->
        Rename("Rename transcription", t.title, close = { dialogs.renaming = null }) { Scribe.rename(t.id, it) }
    }
    dialogs.renamingDictation?.let { d ->
        Rename("Rename dictation", d.title, close = { dialogs.renamingDictation = null }) { Dictations.rename(d.id, it) }
    }
    dialogs.deleting?.let { t ->
        ConfirmDelete(
            "Delete transcription?",
            "\"${t.title}\" and its ${t.count} paragraph(s) will be deleted. This can't be undone.",
            close = { dialogs.deleting = null },
        ) { Scribe.delete(t.id) }
    }
    dialogs.deletingDictation?.let { d ->
        ConfirmDelete(
            "Delete dictation?",
            "\"${d.title}\" and its text will be deleted. This can't be undone.",
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
            ) { Text("Rename") }
        },
        dismissButton = { TextButton(onClick = close) { Text("Cancel") } },
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
            }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = close) { Text("Cancel") } },
    )
}

/** The pages of settings, listed under ⚙. */
enum class SettingsPage(val title: String) {
    MICROPHONE("Microphone"),
    RECORDING("Recording"),
    LANGUAGES("Languages"),
    KEYBOARD("Keyboard"),
    DISPLAY("Display"),
    ABOUT("About"),
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
        title = { Text(page.title) },
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
        confirmButton = { TextButton(onClick = close) { Text("Done") } },
    )
}

/** Which mic, and its gain. Take effect when the mic next starts. */
@Composable
private fun MicrophonePage() {
    Text("Mic", style = MaterialTheme.typography.titleSmall)
    MicRoute.entries.forEach { r ->
        Choice(r.label, Scribe.route == r) { Scribe.chooseRoute(r) }
    }
    Text(
        "Mic boost",
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(top = 12.dp),
    )
    Boost.entries.forEach { b ->
        val label = if (b == Boost.AUTO) "Auto (raise quiet speech)" else b.label
        Choice(label, Scribe.boost == b) { Scribe.chooseBoost(b) }
    }
    if (Scribe.recording) {
        Text(
            "Changes apply when the mic next starts (pause and resume).",
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
        "Keep audio (.opus)",
        "Save the audio with the transcript. Afterwards, tap a word to hear it from there.",
        Scribe.keepAudio,
        Scribe::chooseKeepAudio,
    )
    Toggle(
        "Keep non-speech audio",
        "Also keep recordings in which no words were detected.",
        Scribe.keepNonSpeech,
        Scribe::chooseKeepNonSpeech,
        enabled = Scribe.keepAudio,
    )
    Toggle(
        "Start recording on New",
        "A new dictation or transcription starts listening as soon as it opens.",
        Scribe.autoStart,
        Scribe::chooseAutoStart,
    )
}

@Composable
private fun KeyboardPage() {
    val context = androidx.compose.ui.platform.LocalContext.current
    Text(
        "Turn on \"Scribe voice typing\" to dictate into other apps, then pick it with the keyboard switcher.",
        style = MaterialTheme.typography.bodyMedium,
    )
    TextButton(onClick = {
        context.startActivity(
            android.content.Intent(android.provider.Settings.ACTION_INPUT_METHOD_SETTINGS)
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }) { Text("Keyboard settings") }
}

/**
 * The language you read in, the ones to listen out for, and what the
 * transcript shows of a paragraph that has a reading.
 *
 * Each other language is one model, downloaded here, that reads it in
 * the language you read. Which languages can be offered is not a matter
 * of taste: a paragraph's language is read off its alphabet, so one
 * sharing the alphabet you read in could never be told apart from your
 * own, and two sharing each other's could not be told apart either. So
 * the list is the languages with a model into yours, and turning one on
 * turns off whichever other one shares its alphabet.
 */
@Composable
private fun LanguagesPage() {
    val installing = InstallService.state.collectAsState().value
    // Recomposes when a model is installed or deleted.
    val models = Scribe.languageModels
    val native = Scribe.native
    Text("I read in", style = MaterialTheme.typography.titleSmall)
    NATIVE_LANGUAGES.forEach { l ->
        Choice(l.label, native == l) { Scribe.chooseNative(l) }
    }
    Text(
        "Everything heard in another language is read into this one.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )

    Text(
        "Live text in",
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(top = 16.dp),
    )
    NATIVE_LANGUAGES.forEach { l ->
        Choice(l.label, Scribe.pass1 == l) { Scribe.choosePass1(l) }
    }
    Text(
        "Pass 1 shows words as they are spoken, and it is the one pass that " +
            "has to be told which language to listen in -- the finished text is " +
            "worked out from the audio, in any of 25. Set this to the language " +
            "being spoken: the one you read in, or one you are listening out for.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Scribe.pass1Missing?.let { l ->
        Text(
            "The ${l.label} live-text model isn't here yet, so the live text is " +
                "in English until it has downloaded.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(top = 4.dp),
        )
    }

    Text(
        "Also listen out for",
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
            l.label,
            when {
                clash != null -> "${l.script.label}, like ${clash.label}: only one of them can be told apart."
                have -> "Downloaded, $mb MB."
                on -> "$mb MB to download."
                else -> "$mb MB, downloaded when you turn it on."
            },
            on,
            { Scribe.chooseOther(l, it) },
            enabled = clash == null && !Scribe.busy(),
        )
    }
    if (translatable(native).isEmpty()) {
        Text(
            "No other language has a model that reads into ${native.label} yet.",
            style = MaterialTheme.typography.bodySmall,
        )
    }

    // What the choices above still want fetched, and what they left behind.
    val missing = remember(models, chosen, native) { Scribe.wanted().filterNot(Scribe.store::installed) }
    val spare = remember(models, chosen, native) { Scribe.spare() }
    when {
        installing is Install.Running -> Text(
            describe(installing.step),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(top = 8.dp),
        )
        installing is Install.Failed -> Text(
            "Download failed: ${installing.error}\nTrying again resumes where it stopped.",
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(top = 8.dp),
        )
        missing.isNotEmpty() -> Text(
            "${missing.sumOf { it.approxBytes } / 1_000_000} MB to download. " +
                "Wi-Fi recommended; it continues if you switch apps.",
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(top = 8.dp),
        )
    }
    if (missing.isNotEmpty() && installing !is Install.Running) {
        TextButton(onClick = Scribe::installLanguages) {
            Text(if (installing is Install.Failed) "Retry" else "Download")
        }
    }
    spare.forEach { m ->
        TextButton(onClick = { Scribe.removeModel(m) }) {
            Text("Delete ${m.dir.removePrefix("translate/")} (${m.approxBytes / 1_000_000} MB)")
        }
    }

    Text(
        "Show",
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(top = 16.dp),
    )
    Show.entries.forEach { s ->
        Choice(s.label, Scribe.showing == s) { Scribe.chooseShowing(s) }
    }
    Text(
        "For paragraphs that were read in another language. What was said " +
            "is always what is kept, and what playback and editing are about.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    if (Scribe.recording || Scribe.busy()) {
        Text(
            "Language changes apply to the next recording.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 8.dp),
        )
    }
}

/** Transcript text size: five steps, the default in the middle. */
@Composable
private fun DisplayPage() {
    Text("Font size", style = MaterialTheme.typography.titleSmall)
    androidx.compose.material3.Slider(
        value = Scribe.fontStep.toFloat(),
        onValueChange = { Scribe.chooseFontStep(it.roundToInt()) },
        valueRange = 0f..(FONT_SCALES.size - 1).toFloat(),
        steps = FONT_SCALES.size - 2,
    )
    Text(
        "The quick brown fox jumps over the lazy dog.",
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
