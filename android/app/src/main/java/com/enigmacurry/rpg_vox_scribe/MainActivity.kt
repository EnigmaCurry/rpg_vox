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
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.foundation.text.selection.DisableSelection
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Scribe.init(this)
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
    var settings by remember { mutableStateOf(false) }
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
                        if (ready) IconButton(onClick = { settings = true }) { Text("⚙") }
                    },
                    // The open thread; tap to rename it.
                    title = {
                        if (ready) {
                            Text(
                                Scribe.thread.title,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.clickable { dialogs.renaming = Scribe.thread },
                            )
                        } else {
                            Text("Scribe")
                        }
                    },
                )
            },
        ) { pad ->
            Box(Modifier.padding(pad).fillMaxSize()) {
                when (val phase = Scribe.phase) {
                    is Phase.NeedModels -> Setup(phase)
                    is Phase.Installing -> Installing(phase)
                    Phase.Loading -> Centered("Loading speech models…", progress = null)
                    Phase.Unloaded -> Unloaded()
                    is Phase.Failed -> Centered("Could not load the models:\n${phase.error}", progress = null)
                    Phase.Ready -> Dictate()
                }
            }
        }
    }
    ThreadDialogHost(dialogs)
    if (settings) MicSettings(close = { settings = false })
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
        Text("Speech models", style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(12.dp))
        Text(
            "Scribe transcribes on the phone. It needs two models from the " +
                "sherpa-onnx releases on GitHub: Parakeet TDT 0.6B v2 (English) and a " +
                "streaming Zipformer for live text. That's a one-time download of about " +
                "$mb MB, kept in the app's private storage. Wi-Fi recommended.",
            style = MaterialTheme.typography.bodyMedium,
        )
        if (error != null) {
            Spacer(Modifier.height(12.dp))
            Text(
                "Download failed: $error\nTrying again resumes where it stopped.",
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Spacer(Modifier.height(24.dp))
        Button(onClick = go, modifier = Modifier.fillMaxWidth()) {
            Text(
                when {
                    error != null -> "Retry"
                    phase.resume -> "Resume download"
                    else -> "Download models"
                },
            )
        }
    }
}

@Composable
private fun Installing(phase: Phase.Installing) {
    Centered(
        "${describe(phase.step)}\n\nThis continues if you switch apps.",
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
        Text("Speech models are unloaded to free memory.", style = MaterialTheme.typography.bodyLarge)
        Spacer(Modifier.height(16.dp))
        Button(onClick = Scribe::load) { Text("Load models") }
    }
}

@Composable
private fun KeepScreenOn() {
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
            Toast.makeText(context, "Microphone permission is needed", Toast.LENGTH_LONG).show()
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
    if (Scribe.recording) KeepScreenOn()
    val job = Scribe.file
    val playing = Playback.playing != null

    Column(Modifier.fillMaxSize()) {
        Transcript(Modifier.weight(1f).fillMaxWidth())
        Surface(tonalElevation = 3.dp) {
            Column(
                Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
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
                val status = when {
                    playing && Playback.paused -> "Playback paused"
                    playing -> "Tap the text to pause"
                    job != null -> "Transcribing ${job.name}" +
                        if (job.progress >= 0) " · ${(job.progress * 100).toInt()}%" else "…"
                    Scribe.finishing -> "Finishing…"
                    Scribe.paused -> "Paused · $time"
                    Scribe.recording -> "Listening · $time" + (Scribe.micName?.let { " · $it" } ?: "")
                    Scribe.paragraphs.any { it.playable() } -> "Tap to start · long-press a word to play it"
                    else -> "Tap to start · keeps going in the background"
                }
                if (playing) {
                    PlaybackBar(now)
                    Spacer(Modifier.height(8.dp))
                } else if (Build.VERSION.SDK_INT >= 29) {
                    SourcePicker(enabled = !Scribe.busy())
                    Spacer(Modifier.height(8.dp))
                }
                Text(status, style = MaterialTheme.typography.labelLarge, maxLines = 1)
                Spacer(Modifier.height(8.dp))
                LinearProgressIndicator(
                    progress = {
                        when {
                            job != null -> job.progress.coerceAtLeast(0f)
                            Scribe.recording -> (Scribe.level * 8f).coerceIn(0f, 1f)
                            else -> 0f
                        }
                    },
                    modifier = Modifier.fillMaxWidth(0.6f),
                    gapSize = 0.dp,
                    drawStopIndicator = {},
                )
                Spacer(Modifier.height(12.dp))
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    val idle = !Scribe.busy()
                    val hasText = Scribe.paragraphs.isNotEmpty()
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        TextButton(onClick = { copy(context, Scribe.text()) }, enabled = hasText) { Text("Copy") }
                        TextButton(onClick = { share(context, Scribe.text()) }, enabled = hasText) { Text("Share") }
                    }
                    RecordButton(
                        recording = Scribe.recording || job != null,
                        enabled = !Scribe.finishing,
                        onClick = toggle,
                    )
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Row {
                            OutlinedButton(
                                onClick = { if (Scribe.paused) Scribe.resume() else Scribe.pause() },
                                enabled = Scribe.recording && !Scribe.finishing,
                            ) { Text(if (Scribe.paused) "▶" else "❚❚") }
                            Spacer(Modifier.size(4.dp))
                            OutlinedButton(
                                onClick = Scribe::breakParagraph,
                                enabled = Scribe.recording && !Scribe.paused,
                            ) { Text("¶") }
                        }
                        TextButton(onClick = Scribe::clear, enabled = idle && hasText) { Text("Clear") }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SourcePicker(enabled: Boolean) {
    val modes = listOf(Mode.MIC to "Mic", Mode.PHONE to "Phone", Mode.BOTH to "Both")
    SingleChoiceSegmentedButtonRow {
        modes.forEachIndexed { i, (m, label) ->
            SegmentedButton(
                selected = Scribe.mode == m,
                onClick = { Scribe.choose(m) },
                enabled = enabled,
                shape = SegmentedButtonDefaults.itemShape(i, modes.size),
            ) { Text(label, maxLines = 1) }
        }
    }
}

/** In place of the source picker while a recording plays: pause/resume, stop, and where it is. */
@Composable
private fun PlaybackBar(now: Long) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        FilledTonalButton(onClick = { if (Playback.paused) Playback.resume() else Playback.pause() }) {
            Text(if (Playback.paused) "▶ Resume" else "❚❚ Pause")
        }
        Spacer(Modifier.size(8.dp))
        OutlinedButton(onClick = Playback::stop) { Text("■ Stop") }
        Spacer(Modifier.size(12.dp))
        val length = Playback.lengthMs
        Text(
            clock(now) + if (length > 0) " / ${clock(length)}" else "",
            style = MaterialTheme.typography.labelLarge,
        )
    }
}

@Composable
private fun RecordButton(recording: Boolean, enabled: Boolean, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = enabled,
        shape = CircleShape,
        modifier = Modifier.size(88.dp).semantics {
            contentDescription = if (recording) "Stop dictation" else "Start dictation"
        },
        colors = androidx.compose.material3.ButtonDefaults.buttonColors(
            containerColor = Color(0xFFD32F2F),
            contentColor = Color.White,
        ),
    ) {
        if (recording) {
            Box(Modifier.size(28.dp).background(Color.White, RoundedCornerShape(4.dp)))
        } else {
            Box(Modifier.size(30.dp).background(Color.White, CircleShape))
        }
    }
}

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
    val last = Scribe.paragraphs.lastOrNull()
    LaunchedEffect(Scribe.paragraphs.size, last, follow) {
        if (follow && Scribe.paragraphs.isNotEmpty()) {
            // Past the last item's top, clamped to the very end.
            list.scrollToItem(Scribe.paragraphs.size - 1, Int.MAX_VALUE)
        }
    }
    if (Scribe.paragraphs.isEmpty()) {
        Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
            Text(
                "Your words appear here. Live text is shown faint, then replaced by the " +
                    "final transcription a moment later.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }
    val faint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.45f)
    Box(
        modifier.pointerInput(playing != null && !held) {
            if (playing == null || held) return@pointerInput
            // Tap anywhere to pause. Watched before the text and the list
            // see it, without taking it from them; a scroll or long-press
            // isn't a tap.
            awaitEachGesture {
                val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
                var moved = false
                while (true) {
                    val c = awaitPointerEvent(PointerEventPass.Initial).changes.firstOrNull { it.id == down.id } ?: break
                    if ((c.position - down.position).getDistance() > viewConfiguration.touchSlop) moved = true
                    if (!c.pressed) {
                        if (!moved && c.uptimeMillis - down.uptimeMillis < viewConfiguration.longPressTimeoutMillis) {
                            Playback.pause()
                        }
                        break
                    }
                }
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
                    // Label each run of paragraphs from one speaker.
                    val prev = Scribe.paragraphs.getOrNull(i - 1)
                    if (p.source != null && prev?.source != p.source) {
                        Text(
                            p.source,
                            style = MaterialTheme.typography.labelLarge,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.padding(bottom = 4.dp),
                        )
                    }
                    if (p.playable()) {
                        DisableSelection {
                            PlayableParagraph(p, playing, at, canPlay = !Scribe.busy(), follow = !held)
                        }
                    } else {
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
                            style = MaterialTheme.typography.bodyLarge,
                        )
                    }
                }
            }
        }
        if (!follow && playing == null) {
            FilledTonalButton(
                onClick = { follow = true },
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 12.dp),
            ) { Text("↓ Jump to latest") }
        }
    }
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
private fun Para.wordAt(i: Int, shown: Int): Word =
    if (words.size == shown) words[i] else words[(i.toLong() * words.size / shown).toInt().coerceIn(0, words.size - 1)]

/**
 * A paragraph with kept audio: long-press a word to play from it, and
 * while its recording plays, the words already heard are lit and the
 * current one highlighted, kept in view.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PlayableParagraph(p: Para, playing: Playing?, at: Karaoke?, canPlay: Boolean, follow: Boolean) {
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
    val haptics = LocalHapticFeedback.current
    val requester = remember { BringIntoViewRequester() }
    // Keep the line being heard in view, with a couple of lines below it.
    val line = if (at?.para == p.id) layout?.getLineForOffset(starts.getOrElse(at.word) { 0 }) else null
    LaunchedEffect(line, follow) {
        val l = layout ?: return@LaunchedEffect
        if (line == null || !follow) return@LaunchedEffect
        val below = minOf(line + 2, l.lineCount - 1)
        requester.bringIntoView(Rect(0f, l.getLineTop(line), l.size.width.toFloat(), l.getLineBottom(below)))
    }
    Text(
        text,
        style = MaterialTheme.typography.bodyLarge,
        onTextLayout = { layout = it },
        modifier = Modifier
            .bringIntoViewRequester(requester)
            .pointerInput(p, canPlay) {
                if (!canPlay) return@pointerInput
                detectTapGestures(onLongPress = { pos ->
                    val l = layout ?: return@detectTapGestures
                    val c = l.getOffsetForPosition(pos)
                    val i = starts.indexOfLast { it <= c }.coerceAtLeast(0)
                    haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                    Scribe.play(p, p.wordAt(i, shown.size))
                })
            },
    )
}

private fun clock(ms: Long): String {
    val s = ms / 1000
    return if (s >= 3600) "%d:%02d:%02d".format(s / 3600, s / 60 % 60, s % 60) else "%d:%02d".format(s / 60, s % 60)
}

private fun copy(context: android.content.Context, text: String) {
    val cm = context.getSystemService(ClipboardManager::class.java)
    cm.setPrimaryClip(ClipData.newPlainText("Transcript", text))
    // Android 13+ shows its own confirmation.
    if (Build.VERSION.SDK_INT < 33) Toast.makeText(context, "Copied", Toast.LENGTH_SHORT).show()
}

private fun share(context: android.content.Context, text: String) {
    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
    context.startActivity(Intent.createChooser(send, "Share transcript"))
}
