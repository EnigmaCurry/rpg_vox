package com.enigmacurry.rpg_vox_scribe

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.app.Activity
import android.content.Intent
import android.media.projection.MediaProjectionConfig
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.contextmenu.builder.item
import androidx.compose.foundation.text.contextmenu.modifier.appendTextContextMenuComponents
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.foundation.combinedClickable
import androidx.compose.ui.draw.clip
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.foundation.Canvas
import androidx.compose.ui.graphics.Path
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.ui.input.pointer.AwaitPointerEventScope
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.geometry.Offset
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.DragInteraction
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Button
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.rememberDrawerState
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.text.style.TextOverflow
import kotlinx.coroutines.launch
import androidx.compose.runtime.setValue
import kotlinx.coroutines.delay
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat

/**
 * Finished paragraphs waiting to be read before the status line says
 * the reading is behind. One is ordinary -- a paragraph that just
 * closed is read a moment later; two is the phone not keeping up.
 */
private const val BEHIND = 2

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Scribe.init(this)
        // Edit and Cut join the text selection menu (Paragraph).
        @OptIn(ExperimentalFoundationApi::class)
        androidx.compose.foundation.ComposeFoundationFlags.isNewContextMenuEnabled = true
        enableEdgeToEdge()
        setContent { AppTheme { App() } }
        if (savedInstanceState == null) handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    /** Share to Scribe / Open with Scribe: transcribe the file. */
    private fun handle(intent: Intent?) {
        val uri: Uri = when (intent?.action) {
            Intent.ACTION_SEND ->
                if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
                } else {
                    @Suppress("DEPRECATION") intent.getParcelableExtra(Intent.EXTRA_STREAM)
                }
            Intent.ACTION_VIEW -> intent.data
            else -> null
        } ?: return
        Scribe.transcribe(uri)?.let { Toast.makeText(this, it, Toast.LENGTH_LONG).show() }
    }

    // Dictation keeps going in the background (ScribeService); this only
    // tracks visibility, which loads the models and times their release.
    override fun onStart() {
        super.onStart()
        Scribe.onVisible(true)
    }

    override fun onStop() {
        super.onStop()
        Dictations.onHidden()
        Scribe.onVisible(false)
    }
}

@Composable
private fun AppTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val colors: ColorScheme = when {
        Build.VERSION.SDK_INT >= 31 && dark -> dynamicDarkColorScheme(context)
        Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(context)
        dark -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(colorScheme = colors, content = content)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun App() {
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val ui = rememberCoroutineScope()
    val dialogs = remember { ThreadDialogs() }
    var settings by remember { mutableStateOf<SettingsPage?>(null) }
    var menu by remember { mutableStateOf(false) }
    val ready = Scribe.phase == Phase.Ready
    // Back closes the drawer before it leaves the app.
    BackHandler(enabled = drawer.isOpen) { ui.launch { drawer.close() } }
    ModalNavigationDrawer(
        drawerState = drawer,
        gesturesEnabled = ready,
        drawerContent = { ThreadDrawer(dialogs, close = { ui.launch { drawer.close() } }) },
    ) {
        Scaffold(
            topBar = {
                TopAppBar(
                    navigationIcon = {
                        if (ready) {
                            IconButton(onClick = { ui.launch { drawer.open() } }) { Text("☰") }
                        }
                    },
                    actions = {
                        if (ready) {
                            Box {
                                IconButton(onClick = { menu = true }) {
                                    androidx.compose.material3.Icon(
                                        DockIcons.More,
                                        contentDescription = stringResource(R.string.a11y_more),
                                    )
                                }
                                androidx.compose.material3.DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                    // The open dictation or transcription, then the settings.
                                    val dictation = Dictations.open
                                    val context = LocalContext.current
                                    val text = if (dictation != null) Dictations.text() else Scribe.text()
                                    androidx.compose.material3.DropdownMenuItem(
                                        text = { Text(stringResource(R.string.action_share)) },
                                        enabled = text.isNotBlank(),
                                        onClick = {
                                            menu = false
                                            share(context, text)
                                        },
                                    )
                                    androidx.compose.material3.DropdownMenuItem(
                                        text = { Text(stringResource(R.string.action_rename)) },
                                        onClick = {
                                            menu = false
                                            if (dictation != null) dialogs.renamingDictation = dictation else dialogs.renaming = Scribe.thread
                                        },
                                    )
                                    androidx.compose.material3.DropdownMenuItem(
                                        text = { Text(stringResource(R.string.action_delete)) },
                                        enabled = dictation != null || !Scribe.busy(),
                                        onClick = {
                                            menu = false
                                            if (dictation != null) {
                                                dialogs.deletingDictation = dictation
                                            } else {
                                                dialogs.deleting = Scribe.threadList.firstOrNull { it.id == Scribe.thread.id } ?: Scribe.thread
                                            }
                                        },
                                    )
                                    androidx.compose.material3.HorizontalDivider()
                                    SettingsPage.entries.forEach { page ->
                                        androidx.compose.material3.DropdownMenuItem(
                                            text = { Text(stringResource(page.titleRes)) },
                                            onClick = {
                                                menu = false
                                                settings = page
                                            },
                                        )
                                    }
                                }
                            }
                        }
                    },
                    // The open thread; tap to rename it.
                    title = {
                        val dictation = Dictations.open
                        if (ready && dictation != null) {
                            Text(
                                dictation.title,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.clickable { dialogs.renamingDictation = dictation },
                            )
                        } else if (ready) {
                            Text(
                                Scribe.thread.title,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.clickable { dialogs.renaming = Scribe.thread },
                            )
                        } else {
                            Text(stringResource(R.string.app_name))
                        }
                    },
                )
            },
        ) { pad ->
            Box(Modifier.padding(pad).fillMaxSize()) {
                when (val phase = Scribe.phase) {
                    is Phase.NeedModels -> Setup(phase)
                    is Phase.Installing -> Installing(phase)
                    Phase.Loading -> Centered(stringResource(R.string.phase_loading), progress = null)
                    Phase.Unloaded -> Unloaded()
                    is Phase.Failed ->
                        Centered(stringResource(R.string.phase_failed, phase.error), progress = null)
                    Phase.Ready -> if (Dictations.open != null) DictationScreen() else Dictate()
                }
            }
        }
    }
    ThreadDialogHost(dialogs)
    settings?.let { Settings(it, close = { settings = null }) }
}

@Composable
private fun Centered(text: String, progress: Float?) {
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(text, style = MaterialTheme.typography.bodyLarge)
        Spacer(Modifier.height(16.dp))
        if (progress == null) {
            LinearProgressIndicator(Modifier.fillMaxWidth())
        } else {
            LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
        }
    }
}

@Composable
private fun Setup(phase: Phase.NeedModels) {
    val error = phase.error
    // The download runs in a foreground service; its notification needs this on 13+.
    val askNotify = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        Scribe.install()
    }
    val context = LocalContext.current
    val go = {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            askNotify.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            Scribe.install()
        }
    }
    val mb = BUNDLES.sumOf { it.approxBytes } / 1_000_000
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(stringResource(R.string.setup_title), style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(12.dp))
        Text(stringResource(R.string.setup_body, mb), style = MaterialTheme.typography.bodyMedium)
        if (error != null) {
            Spacer(Modifier.height(12.dp))
            Text(
                stringResource(R.string.download_failed, error),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Spacer(Modifier.height(24.dp))
        Button(onClick = go, modifier = Modifier.fillMaxWidth()) {
            Text(
                stringResource(
                    when {
                        error != null -> R.string.action_retry
                        phase.resume -> R.string.setup_resume
                        else -> R.string.setup_download
                    },
                ),
            )
        }
    }
}

@Composable
private fun Installing(phase: Phase.Installing) {
    val context = LocalContext.current
    Centered(
        stringResource(R.string.installing_background, describe(context, phase.step)),
        progress = phase.overall,
    )
}

@Composable
private fun Unloaded() {
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(stringResource(R.string.unloaded_body), style = MaterialTheme.typography.bodyLarge)
        Spacer(Modifier.height(16.dp))
        Button(onClick = Scribe::load) { Text(stringResource(R.string.unloaded_load)) }
    }
}

@Composable
fun KeepScreenOn() {
    val view = LocalView.current
    DisposableEffect(Unit) {
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = false }
    }
}

@Composable
private fun Dictate() {
    val context = LocalContext.current
    // Phone audio: Android's screen-sharing consent, once per recording.
    val consent = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
        val data = r.data
        if (r.resultCode == Activity.RESULT_OK && data != null) Scribe.start(r.resultCode to data)
    }
    val begin = {
        if (Scribe.mode.phone) {
            val mpm = context.getSystemService(MediaProjectionManager::class.java)
            // Whole device: a single app's projection would only capture that app's audio.
            consent.launch(
                if (Build.VERSION.SDK_INT >= 34) {
                    mpm.createScreenCaptureIntent(MediaProjectionConfig.createConfigForDefaultDisplay())
                } else {
                    mpm.createScreenCaptureIntent()
                },
            )
        } else {
            Scribe.start()
        }
    }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { got ->
        if (got[Manifest.permission.RECORD_AUDIO] != false) {
            begin()
        } else {
            Toast.makeText(context, context.getString(R.string.toast_need_mic), Toast.LENGTH_LONG).show()
        }
    }
    val toggle = {
        // Phone audio is recorded under RECORD_AUDIO too; the notification
        // (with Pause/Stop) needs its own permission on Android 13+.
        val wanted = listOfNotNull(
            Manifest.permission.RECORD_AUDIO,
            if (Build.VERSION.SDK_INT >= 33) Manifest.permission.POST_NOTIFICATIONS else null,
        ).filter { ContextCompat.checkSelfPermission(context, it) != PackageManager.PERMISSION_GRANTED }
        when {
            Scribe.recording || Scribe.file != null -> Scribe.stop()
            wanted.isEmpty() -> begin()
            else -> ask.launch(wanted.toTypedArray())
        }
    }
    // Made with New, with "Start recording on New" on.
    LaunchedEffect(Scribe.startWhenOpen) {
        if (Scribe.startWhenOpen != "transcription") return@LaunchedEffect
        Scribe.startWhenOpen = null
        if (!Scribe.busy()) toggle()
    }
    if (Scribe.recording) KeepScreenOn()
    val job = Scribe.file
    val playing = Playback.playing != null

    Column(Modifier.fillMaxSize()) {
        Transcript(Modifier.weight(1f).fillMaxWidth())
        // Ticks the elapsed time once a second while a session is open.
        var now by remember { mutableLongStateOf(0L) }
        LaunchedEffect(Scribe.recording) {
            while (Scribe.recording) {
                now = Scribe.elapsedMs()
                delay(1_000 - now % 1_000)
            }
        }
        LaunchedEffect(playing) {
            while (Playback.playing != null) {
                now = Playback.positionMs()
                delay(250)
            }
        }
        val time = clock(if (Scribe.recording) now else 0)
        val hasText = Scribe.paragraphs.isNotEmpty()
        // Read once: the status line names the language and counts the
        // paragraphs, and both are getters over state another thread moves.
        val missing = Scribe.pass1Missing
        val behind = Scribe.readingBehind
        val copyButton: @Composable () -> Unit = {
            DockButton(DockIcons.Copy, stringResource(R.string.action_copy), enabled = hasText) {
                copy(context, Scribe.text())
            }
        }
        when {
            // Playback: its own controls; Stop leaves it.
            playing -> Dock(
                status = stringResource(
                    if (Playback.paused) R.string.dock_playing_paused else R.string.dock_playing,
                    playTime(now),
                ),
                live = false,
                l2 = {
                    DockButton(DockIcons.Stop, stringResource(R.string.a11y_stop_playing), onClick = Playback::stop)
                },
                l1 = {
                    DockButton(DockIcons.Back, stringResource(R.string.a11y_back_10)) { Playback.skip(-10_000) }
                },
                center = {
                    BigButton(
                        label = stringResource(if (Playback.paused) R.string.action_resume else R.string.action_pause),
                        enabled = true,
                        onClick = { if (Playback.paused) Playback.resume() else Playback.pause() },
                        icon = if (Playback.paused) DockIcons.Play else DockIcons.Pause,
                        ring = if (Playback.lengthMs > 0) now.toFloat() / Playback.lengthMs else 0f,
                    )
                },
                r1 = {
                    DockButton(DockIcons.Ahead, stringResource(R.string.a11y_ahead_10)) { Playback.skip(10_000) }
                },
                r2 = copyButton,
            )
            // Recording, or transcribing a file.
            Scribe.busy() -> Dock(
                status = when {
                    job != null -> stringResource(R.string.transcribing_file, job.name) +
                        (
                            job.detail(context)?.let { " · $it" }
                                ?: if (job.progress >= 0) " · ${(job.progress * 100).toInt()}%" else "…"
                            )
                    Scribe.finishing -> stringResource(R.string.status_finishing)
                    Scribe.paused -> stringResource(R.string.dock_paused_at, time)
                    // A language pass 1 cannot listen in yet, because
                    // its model is still downloading. A recording is
                    // where that matters, so it is said here.
                    missing != null ->
                        stringResource(R.string.dock_listening_missing, time, stringResource(missing.nameRes))
                    // The reading has fallen behind the speaking, which
                    // it does when Scribe spends the recording off
                    // screen: it says so instead of the readings simply
                    // not arriving. Said here rather than beside the
                    // mic, which matters less than this does.
                    behind >= BEHIND ->
                        pluralStringResource(R.plurals.dock_listening_behind, behind, time, behind)
                    else -> Scribe.micName
                        ?.let { stringResource(R.string.dock_listening_on, time, it) }
                        ?: stringResource(R.string.dock_listening, time)
                },
                live = Scribe.recording && !Scribe.paused && !Scribe.finishing,
                l2 = null,
                l1 = null,
                center = {
                    BigButton(
                        label = stringResource(
                            if (job != null) R.string.action_stop_transcribing else R.string.action_stop_recording,
                        ),
                        enabled = !Scribe.finishing,
                        onClick = toggle,
                        recording = true,
                        ring = when {
                            job != null -> job.progress.coerceAtLeast(0f)
                            Scribe.recording && !Scribe.paused -> Scribe.level * 8f
                            else -> 0f
                        },
                    )
                },
                r1 = if (Scribe.recording && !Scribe.finishing) {
                    {
                        DockButton(
                            if (Scribe.paused) DockIcons.Play else DockIcons.Pause,
                            stringResource(if (Scribe.paused) R.string.action_resume else R.string.action_pause),
                        ) { if (Scribe.paused) Scribe.resume() else Scribe.pause() }
                    }
                } else {
                    null
                },
                r2 = copyButton,
            )
            else -> Dock(
                status = stringResource(
                    if (Scribe.paragraphs.any { it.playable() }) {
                        R.string.dock_hint_record_and_play
                    } else {
                        R.string.dock_hint_record
                    },
                ),
                live = false,
                l2 = if (Build.VERSION.SDK_INT >= 29) { { SourceButton() } } else null,
                l1 = {
                    // Also clears audio that has no text with it.
                    var confirm by remember { mutableStateOf(false) }
                    DockButton(
                        DockIcons.Clear,
                        stringResource(R.string.action_clear),
                        enabled = hasText || Scribe.threadAudio.isNotEmpty(),
                    ) { confirm = true }
                    if (confirm) ConfirmClear(close = { confirm = false })
                },
                center = {
                    BigButton(label = stringResource(R.string.action_start_recording), enabled = true, onClick = toggle)
                },
                r1 = null,
                r2 = copyButton,
            )
        }
    }
}

/** What to record from (Mic, Phone, Both): its icon, tapped for a menu. */
@Composable
private fun SourceButton() {
    var menu by remember { mutableStateOf(false) }
    val modes = listOf(
        Triple(Mode.MIC, stringResource(R.string.source_mic), DockIcons.Mic),
        Triple(Mode.PHONE, stringResource(R.string.source_phone), DockIcons.Phone),
        Triple(Mode.BOTH, stringResource(R.string.source_both), DockIcons.Both),
    )
    val current = modes.first { it.first == Scribe.mode }
    Box {
        DockButton(current.third, stringResource(R.string.a11y_record_from, current.second)) { menu = true }
        androidx.compose.material3.DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            modes.forEach { (m, label, icon) ->
                androidx.compose.material3.DropdownMenuItem(
                    text = { Text(label) },
                    leadingIcon = { androidx.compose.material3.Icon(icon, null) },
                    trailingIcon = { if (m == Scribe.mode) Text("✓") },
                    onClick = {
                        menu = false
                        Scribe.choose(m)
                    },
                )
            }
        }
    }
}


/** Play [recording] from 0:00: small above its first paragraph, or [big] when it has no words. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PlayFromStart(recording: String, big: Boolean) {
    val size = if (big) 96.dp else 36.dp
    // Tap plays; long-press offers to delete this one recording.
    var menu by remember { mutableStateOf(false) }
    var confirm by remember { mutableStateOf(false) }
    val haptics = LocalHapticFeedback.current
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .padding(bottom = if (big) 0.dp else 4.dp)
            .size(size)
            .background(Color(0xFF2E7D32), CircleShape)
            .clip(CircleShape)
            .combinedClickable(
                onClickLabel = stringResource(R.string.a11y_play_from_start),
                onLongClickLabel = stringResource(R.string.a11y_recording_options),
                onLongClick = {
                    haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                    menu = true
                },
                onClick = { Scribe.playFromStart(recording) },
            ),
    ) {
        androidx.compose.material3.DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            androidx.compose.material3.DropdownMenuItem(
                text = {
                    Text(stringResource(R.string.action_delete_clip), color = MaterialTheme.colorScheme.error)
                },
                onClick = {
                    menu = false
                    confirm = true
                },
            )
        }
        if (confirm) {
            androidx.compose.material3.AlertDialog(
                onDismissRequest = { confirm = false },
                title = { Text(stringResource(R.string.delete_clip_title)) },
                text = { Text(stringResource(R.string.delete_clip_body)) },
                confirmButton = {
                    TextButton(onClick = {
                        confirm = false
                        Scribe.deleteRecording(recording)
                    }) { Text(stringResource(R.string.action_delete), color = MaterialTheme.colorScheme.error) }
                },
                dismissButton = {
                    TextButton(onClick = { confirm = false }) { Text(stringResource(R.string.action_cancel)) }
                },
            )
        }
        Canvas(Modifier.size(size * 0.36f)) {
            drawPath(
                Path().apply {
                    moveTo(this@Canvas.size.width * 0.15f, 0f)
                    lineTo(this@Canvas.size.width, this@Canvas.size.height / 2)
                    lineTo(this@Canvas.size.width * 0.15f, this@Canvas.size.height)
                    close()
                },
                Color.White,
            )
        }
    }
}

/** Asks before [Scribe.clear] empties the open thread. */
@Composable
private fun ConfirmClear(close: () -> Unit) {
    val recordings = Scribe.recordings.size
    androidx.compose.material3.AlertDialog(
        onDismissRequest = close,
        title = { Text(stringResource(R.string.clear_thread_title)) },
        text = {
            Text(
                if (recordings > 0) {
                    pluralStringResource(R.plurals.clear_thread_body_audio, recordings, recordings)
                } else {
                    stringResource(R.string.clear_thread_body)
                },
            )
        },
        confirmButton = {
            TextButton(onClick = {
                Scribe.clear()
                close()
            }) { Text(stringResource(R.string.action_clear), color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = close) { Text(stringResource(R.string.action_cancel)) } },
    )
}

/** A recording in the log with no words: play the thread from its start. */
@Composable
private fun NoWords(recording: String) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(bottom = 8.dp)) {
        if (!Scribe.busy()) PlayFromStart(recording, big = false)
        Spacer(Modifier.size(8.dp))
        Text(
            stringResource(R.string.transcript_no_words),
            style = MaterialTheme.typography.bodyMedium,
            fontStyle = FontStyle.Italic,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** When [recording] began (epoch ms), from its name (yyyyMMdd-HHmmss-…). */
private fun recordingStart(recording: String): Long =
    runCatching {
        java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.ROOT).parse(recording.take(15))!!.time
    }.getOrDefault(0L)

/** Transcript text sizes the Display setting steps through; the middle one is the default. */
val FONT_SCALES = listOf(0.8f, 0.9f, 1f, 1.15f, 1.3f)

/** This style with its font size and line height times [scale]. */
fun TextStyle.scaled(scale: Float): TextStyle =
    copy(fontSize = fontSize * scale, lineHeight = lineHeight * scale)

/** "1:23 / 4:56": where playback is, of how long. */
private fun playTime(now: Long): String =
    clock(now) + Playback.lengthMs.let { if (it > 0) " / ${clock(it)}" else "" }



@Composable
private fun Transcript(modifier: Modifier) {
    val list = rememberLazyListState()
    // Follow the bottom (the newest, still-changing text) until the reader
    // scrolls up; "Jump to latest" or scrolling back down resumes it.
    var follow by remember { mutableStateOf(true) }
    var dragged by remember { mutableStateOf(false) }
    LaunchedEffect(list) {
        list.interactionSource.interactions.collect {
            if (it is DragInteraction.Start) {
                dragged = true
                follow = false
            }
        }
    }
    LaunchedEffect(list) {
        snapshotFlow { list.isScrollInProgress }.collect { scrolling ->
            if (!scrolling && dragged) {
                dragged = false
                follow = !list.canScrollForward
            }
        }
    }
    // Playback: the word being heard, followed down the transcript.
    val playing = Playback.playing
    var at by remember { mutableStateOf<Karaoke?>(null) }
    LaunchedEffect(playing) {
        at = null
        if (playing == null) return@LaunchedEffect
        while (true) {
            withFrameMillis { }
            val k = locate(Scribe.paragraphs, playing, Playback.positionMs())
            if (k != at) at = k
        }
    }
    val held = Playback.paused
    // Paused, the reader scrolls freely; resuming brings the word back.
    LaunchedEffect(at?.para, held) {
        if (held) return@LaunchedEffect
        val i = Scribe.paragraphs.indexOfFirst { it.id == at?.para }
        if (i >= 0 && list.layoutInfo.visibleItemsInfo.none { it.index == i }) list.animateScrollToItem(i)
    }
    // Recordings in which nothing was transcribed, each shown just before
    // the first paragraph said after it started (or at the end).
    val heard = Scribe.paragraphs.mapNotNullTo(HashSet()) { p -> p.audio?.let { Scribe.threadAudio[it] } }
    val before = HashMap<String, MutableList<String>>()
    val tail = mutableListOf<String>()
    // Shown only when non-speech audio is kept; otherwise there's nothing to play.
    for (rec in Scribe.recordings.filter { Scribe.keepNonSpeech && it !in heard }) {
        val start = recordingStart(rec)
        val p = Scribe.paragraphs.firstOrNull { it.atMs > start }
        if (p == null) tail += rec else before.getOrPut(p.id) { mutableListOf() } += rec
    }
    val last = Scribe.paragraphs.lastOrNull()
    LaunchedEffect(Scribe.paragraphs.size, last, tail.size, follow) {
        if (follow && Scribe.paragraphs.isNotEmpty()) {
            // Past the last item's top, clamped to the very end.
            list.scrollToItem(list.layoutInfo.totalItemsCount.coerceAtLeast(1) - 1, Int.MAX_VALUE)
        }
    }
    if (Scribe.paragraphs.isEmpty()) {
        // Kept audio with nothing transcribed still plays, from the start.
        val rec = Scribe.threadAudio.values.maxOrNull()?.takeIf { Scribe.keepNonSpeech }
        Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
            if (rec != null && !Scribe.busy()) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    PlayFromStart(rec, big = true)
                    Spacer(Modifier.height(16.dp))
                    Text(
                        stringResource(R.string.transcript_no_words_in_thread),
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                Text(
                    stringResource(R.string.transcript_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        return
    }
    // The first paragraph of each recording, which gets a play-from-start button.
    val firsts = HashMap<String, String>()
    for (p in Scribe.paragraphs) {
        val rec = Scribe.threadAudio[p.audio ?: continue] ?: continue
        if (rec !in firsts.values) firsts[p.id] = rec
    }
    val faint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.45f)
    Box(
        modifier.pointerInput(playing != null && !held) {
            if (playing == null || held) return@pointerInput
            // Tap anywhere to pause. Watched before the text and the list
            // see it, without taking it from them; a scroll or long-press (selection)
            // isn't a tap.
            awaitEachGesture {
                if (awaitTap(PointerEventPass.Initial) != null) Playback.pause()
            }
        },
    ) {
        SelectionContainer(Modifier.fillMaxSize()) {
            LazyColumn(
                state = list,
                modifier = Modifier.fillMaxSize(),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                itemsIndexed(Scribe.paragraphs, key = { _, p -> p.id }) { i, p ->
                    before[p.id]?.forEach { NoWords(it) }
                    // Label each run of paragraphs from one speaker.
                    val prev = Scribe.paragraphs.getOrNull(i - 1)
                    if (p.source != null && prev?.source != p.source) {
                        Text(
                            speakerLabel(LocalContext.current, p.source),
                            style = MaterialTheme.typography.labelLarge.scaled(FONT_SCALES[Scribe.fontStep]),
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.padding(bottom = 4.dp),
                        )
                    }
                    val first = firsts[p.id]
                    if (first != null && !Scribe.busy()) PlayFromStart(first, big = false)
                    Paragraph(p, playing, at, canPlay = !Scribe.busy() && p.playable(), follow = !held)
                }
                if (tail.isNotEmpty()) item(key = "wordless") { tail.forEach { NoWords(it) } }
            }
        }
        if (!follow && playing == null) {
            FilledTonalButton(
                onClick = { follow = true },
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 12.dp),
            ) { Text(stringResource(R.string.transcript_jump_to_latest)) }
        }
    }
}

/**
 * A quick tap, without consuming anything: where it was lifted, or null
 * for a drag past the touch slop or a press held to a long-press.
 */
suspend fun AwaitPointerEventScope.awaitTap(pass: PointerEventPass): Offset? {
    val down = awaitFirstDown(requireUnconsumed = false, pass = pass)
    while (true) {
        val c = awaitPointerEvent(pass).changes.firstOrNull { it.id == down.id } ?: return null
        if ((c.position - down.position).getDistance() > viewConfiguration.touchSlop) return null
        if (c.uptimeMillis - down.uptimeMillis >= viewConfiguration.longPressTimeoutMillis) return null
        if (!c.pressed) return c.position
    }
}

private object EditItem
private object CutItem

/** Retype a paragraph; word times carry over where words still match. */
@Composable
private fun EditParagraph(p: Para, close: () -> Unit) {
    var text by remember { mutableStateOf(p.text) }
    androidx.compose.material3.AlertDialog(
        onDismissRequest = close,
        title = { Text(stringResource(R.string.action_edit)) },
        text = {
            androidx.compose.material3.OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(onClick = {
                Scribe.edit(p, text)
                close()
            }) { Text(stringResource(R.string.action_save)) }
        },
        dismissButton = { TextButton(onClick = close) { Text(stringResource(R.string.action_cancel)) } },
    )
}

/** The word being heard: in paragraph [para], word [word], which starts at [ms] on the recording's timeline. */
private data class Karaoke(val para: String, val word: Int, val ms: Long)

/** Has kept audio and word times, and is finished text. */
private fun Para.playable() = audio != null && words.isNotEmpty() && clips.none { it.partial }

/** The last word of [playing] to have started by [pos]. */
private fun locate(paras: List<Para>, playing: Playing, pos: Long): Karaoke? {
    var best: Karaoke? = null
    for (p in paras) {
        val off = playing.offsets[p.audio ?: continue] ?: continue
        val shown = p.shown.size
        for (i in 0 until shown) {
            val t = off + p.wordAt(i, shown).startMs
            if (t <= pos && (best == null || t >= best.ms)) best = Karaoke(p.id, i, t)
        }
    }
    return best
}

/** Timing for shown word [i] of [shown]: one to one when the counts agree, else spread. */
fun Para.wordAt(i: Int, shown: Int): Word =
    if (words.size == shown) words[i] else words[(i.toLong() * words.size / shown).toInt().coerceIn(0, words.size - 1)]

/**
 * A paragraph with kept audio: tap a word to play from it (long-press
 * still selects text), and
 * while its recording plays, the words already heard are lit and the
 * current one highlighted, kept in view.
 */
@OptIn(ExperimentalFoundationApi::class)
/**
 * One paragraph, in the language you read and in no other.
 *
 * A paragraph spoken in your own language is itself ([Scribe.saidAsIs]): the
 * text, with everything it can do -- tap a word to hear it, select to
 * edit or cut -- because that is the text the passes are working on and
 * the one the word timings are of.
 *
 * A paragraph heard in another language is its reading, a sentence at a
 * time, once each sentence has one. What was said is what is kept: it is
 * in the thread's database, it is what every later pass refines, and it
 * is what the reading is remade from each time one of them changes it.
 *
 * What a sentence with no reading yet looks like is a setting, **Show
 * what was said until it is read** (Settings -> Languages): the words as
 * they were heard, faintly, in the language they were heard in, or a
 * faint ellipsis in their place. The reading runs a sentence behind the
 * speaking on purpose (a translator given half a sentence returns half a
 * thought), so there is always one of those while a paragraph is being
 * spoken -- and a paragraph pass 1 is still typing is all of one, until
 * pass 2 has decided it and the translator has read it. Either way the
 * paragraph is visibly still going rather than appearing to have
 * stopped; see [Reading].
 */
@Composable
private fun Paragraph(p: Para, playing: Playing?, at: Karaoke?, canPlay: Boolean, follow: Boolean) {
    if (Scribe.saidAsIs(p)) {
        // A paragraph pass 1 is still adding to is drawn rather than
        // edited: the words it is still typing are not the text any pass
        // has settled, and a field that reported them as selectable
        // would be offering to cut them.
        if (p.clips.any { it.partial }) Saying(p) else Said(p, playing, at, canPlay, follow)
        return
    }
    Reading(p, canPlay)
}

/**
 * A translated paragraph, read in the language you read: its sentences
 * as one paragraph, the way the paragraph they are of was spoken.
 *
 * One block rather than a line per sentence. The sentences arrive one at
 * a time, which is a reason to draw them as they come and no reason at
 * all to leave them stacked -- what was said is a paragraph, and a
 * reading of it that breaks every sentence onto its own line reads like
 * a list of things nobody said that way.
 *
 * There is always something said with no reading yet -- the reading runs
 * a sentence behind the speaking on purpose, a translator given half a
 * sentence returning half a thought -- and that is the sentence being
 * spoken now, one whose reading is still on its way, or the whole
 * paragraph while pass 1 is typing it. With **Show what was said until
 * it is read** on, it is shown as it was said, faintly, in the language
 * it was heard in, and gives way to its reading as that lands; with it
 * off, a faint ellipsis holds the place. Either way it is inline, part
 * of the paragraph rather than a line of its own.
 *
 * Tapping a reading plays from where the sentence tapped was spoken. A
 * translation has its own word count and its own word order, so the
 * sentence is as fine-grained as this can honestly be -- which is why
 * each one's place in the text is kept here. What was said while the
 * reading catches up is no tap target of its own: a tap in it lands on
 * the sentence before it, pass 2 not having settled its own words yet.
 */
@Composable
private fun Reading(p: Para, canPlay: Boolean) {
    val context = LocalContext.current
    val faint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.45f)
    // The engine names the language it heard in English; the screen
    // names it in whichever language is being read.
    val heardIn = p.heardIn?.let { languageName(context, it) }
    val rows = p.rows()
    // The block in order: each reading with when its sentence was
    // spoken, and -- where no reading has landed -- what was said,
    // which has no time of its own (-1) and is drawn faintly.
    val parts = rows.mapNotNull { r ->
        when {
            r.reading != null -> r.reading to r.startMs
            Scribe.showSaid && r.said.isNotBlank() -> r.said to -1L
            else -> null
        }
    }
    // Nothing of it to show: what was said is standing in for no
    // reading, or there is no reading yet and nothing standing in.
    val pending = parts.isEmpty() || !Scribe.showSaid && rows.any { it.reading == null }
    // Where each reading starts in the block, and when it was spoken,
    // for a tap to find it.
    var offset = 0
    val taps = buildList {
        for ((body, ms) in parts) {
            if (ms >= 0) add(offset to ms)
            offset += body.length + 1
        }
    }
    val text = buildAnnotatedString {
        parts.forEachIndexed { i, (body, ms) ->
            if (i > 0) append(' ')
            if (ms >= 0) append(body) else withStyle(SpanStyle(color = faint)) { append(body) }
        }
        if (pending) {
            if (parts.isNotEmpty()) append(' ')
            withStyle(SpanStyle(color = faint)) { append("…") }
        }
    }
    var layout by remember { mutableStateOf<TextLayoutResult?>(null) }
    Text(
        text,
        style = MaterialTheme.typography.bodyLarge.scaled(FONT_SCALES[Scribe.fontStep]),
        onTextLayout = { layout = it },
        modifier = Modifier
            .fillMaxWidth()
            .semantics {
                heardIn?.let { contentDescription = context.getString(R.string.a11y_read_as, it, text) }
            }
            .pointerInput(p.id, canPlay, taps) {
                if (!canPlay) return@pointerInput
                awaitEachGesture {
                    val playingAtDown = Playback.playing != null && !Playback.paused
                    val up = awaitTap(PointerEventPass.Main) ?: return@awaitEachGesture
                    if (playingAtDown || Playback.playing != null && !Playback.paused) {
                        return@awaitEachGesture
                    }
                    val l = layout ?: return@awaitEachGesture
                    val c = l.getOffsetForPosition(up)
                    val i = taps.indexOfLast { it.first <= c }
                    val from = taps.getOrNull(i)?.second ?: return@awaitEachGesture
                    Scribe.play(p, Word("", from, from))
                }
            },
    )
}

/**
 * What was said: the paragraph, with everything it can do -- a word
 * played by tapping it, a selection edited or cut. Only a paragraph in
 * the language you read is shown this way; see [Paragraph].
 */
/**
 * A paragraph with pass 1 still typing into it: what the offline passes
 * have settled, and the words being spoken now after them, faint.
 *
 * Only ever a paragraph in the language you read. One in another
 * language is its reading, and [Paragraph] is where that is decided --
 * this used to be chosen in the transcript instead, which is how the
 * other language stayed on the screen for as long as somebody was
 * speaking it.
 */
@Composable
private fun Saying(p: Para) {
    val faint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.45f)
    Text(
        buildAnnotatedString {
            p.clips.filter { it.text.isNotBlank() }.forEachIndexed { i, c ->
                if (i > 0) append(' ')
                if (c.partial) {
                    // Zipformer partials are ALL CAPS.
                    withStyle(SpanStyle(color = faint, fontStyle = FontStyle.Italic)) {
                        append(c.text.lowercase())
                    }
                } else {
                    append(c.text)
                }
            }
        },
        style = MaterialTheme.typography.bodyLarge.scaled(FONT_SCALES[Scribe.fontStep]),
    )
}

@Composable
private fun Said(p: Para, playing: Playing?, at: Karaoke?, canPlay: Boolean, follow: Boolean) {
    // The selection menu is built outside a composition, so its labels
    // are read here.
    val editLabel = stringResource(R.string.action_edit)
    val cutLabel = stringResource(R.string.action_cut)
    val shown = p.shown
    // Start offset of each shown word in the text.
    val starts = remember(shown) { shown.runningFold(0) { acc, w -> acc + w.length + 1 }.dropLast(1) }
    val off = playing?.offsets?.get(p.audio)
    val heard = MaterialTheme.colorScheme.primary
    val current = SpanStyle(
        color = MaterialTheme.colorScheme.onPrimaryContainer,
        background = MaterialTheme.colorScheme.primaryContainer,
    )
    val text = buildAnnotatedString {
        shown.forEachIndexed { i, w ->
            if (i > 0) append(' ')
            val t = off?.let { it + p.wordAt(i, shown.size).startMs }
            when {
                at == null || t == null -> append(w)
                at.para == p.id && at.word == i -> withStyle(current) { append(w) }
                t < at.ms -> withStyle(SpanStyle(color = heard)) { append(w) }
                else -> append(w)
            }
        }
    }
    var layout by remember { mutableStateOf<TextLayoutResult?>(null) }
    val requester = remember { BringIntoViewRequester() }
    // Keep the line being heard in view, with a couple of lines below it.
    val line = if (at?.para == p.id) layout?.getLineForOffset(starts.getOrElse(at.word) { 0 }) else null
    LaunchedEffect(line, follow) {
        val l = layout ?: return@LaunchedEffect
        if (line == null || !follow) return@LaunchedEffect
        val below = minOf(line + 2, l.lineCount - 1)
        requester.bringIntoView(Rect(0f, l.getLineTop(line), l.size.width.toFloat(), l.getLineBottom(below)))
    }
    // A read-only field rather than plain text: it reports what's selected,
    // which Edit and Cut in the selection menu act on.
    var selection by remember(p.id) { mutableStateOf(TextRange.Zero) }
    val sel = TextRange(selection.start.coerceIn(0, text.length), selection.end.coerceIn(0, text.length))
    var editing by remember(p.id) { mutableStateOf(false) }
    if (editing) EditParagraph(p, close = { editing = false })
    BasicTextField(
        value = TextFieldValue(text, sel),
        onValueChange = { selection = it.selection },
        readOnly = true,
        textStyle = MaterialTheme.typography.bodyLarge.scaled(FONT_SCALES[Scribe.fontStep])
            .copy(color = MaterialTheme.colorScheme.onSurface),
        cursorBrush = SolidColor(Color.Transparent),
        onTextLayout = { layout = it },
        modifier = Modifier
            .fillMaxWidth()
            .appendTextContextMenuComponents {
                if (sel.collapsed) return@appendTextContextMenuComponents
                separator()
                // Edits would be overwritten while the engine still works on the
                // text; cuts are reapplied to its later versions.
                if (!Scribe.busy()) {
                    item(key = EditItem, label = editLabel) {
                        editing = true
                        close()
                    }
                }
                item(key = CutItem, label = cutLabel) {
                    Scribe.cut(p, sel.min, sel.max)
                    selection = TextRange.Zero
                    close()
                }
            }
            .bringIntoViewRequester(requester)
            .pointerInput(p, canPlay) {
                if (!canPlay) return@pointerInput
                // A quick tap, so a long-press stays text selection. Nothing
                // is consumed, leaving the selection and the list their gestures.
                awaitEachGesture {
                    // While playing, a tap pauses instead (see Transcript).
                    val playingAtDown = Playback.playing != null && !Playback.paused
                    val up = awaitTap(PointerEventPass.Main) ?: return@awaitEachGesture
                    if (playingAtDown || Playback.playing != null && !Playback.paused) return@awaitEachGesture
                    val l = layout ?: return@awaitEachGesture
                    val c = l.getOffsetForPosition(up)
                    val i = starts.indexOfLast { it <= c }.coerceAtLeast(0)
                    Scribe.play(p, p.wordAt(i, shown.size))
                }
            },
    )
}


private fun copy(context: android.content.Context, text: String) {
    val cm = context.getSystemService(ClipboardManager::class.java)
    cm.setPrimaryClip(ClipData.newPlainText(context.getString(R.string.clip_transcript), text))
    // Android 13+ shows its own confirmation.
    if (Build.VERSION.SDK_INT < 33) {
        Toast.makeText(context, context.getString(R.string.toast_copied), Toast.LENGTH_SHORT).show()
    }
}

private fun share(context: android.content.Context, text: String) {
    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
    context.startActivity(Intent.createChooser(send, context.getString(R.string.share_transcript)))
}
