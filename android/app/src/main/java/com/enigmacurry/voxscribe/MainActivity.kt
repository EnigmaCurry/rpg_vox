package com.enigmacurry.voxscribe

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.background
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
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Button
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
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
    private val vm: DictationViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { AppTheme { App(vm) } }
    }

    // Dictation runs only while the app is visible (no foreground service).
    override fun onStop() {
        super.onStop()
        vm.stop()
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
private fun App(vm: DictationViewModel) {
    Scaffold(topBar = { TopAppBar(title = { Text("Vox Scribe") }) }) { pad ->
        Box(Modifier.padding(pad).fillMaxSize()) {
            when (val phase = vm.phase) {
                is Phase.NeedModels -> Setup(vm, phase)
                is Phase.Installing -> Installing(phase)
                Phase.Loading -> Centered("Loading speech models…", progress = null)
                is Phase.Failed -> Centered("Could not load the models:\n${phase.error}", progress = null)
                Phase.Ready -> Dictate(vm)
            }
        }
    }
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
private fun Setup(vm: DictationViewModel, phase: Phase.NeedModels) {
    val error = phase.error
    // The download runs in a foreground service; its notification needs this on 13+.
    val askNotify = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        vm.install()
    }
    val context = LocalContext.current
    val go = {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            askNotify.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            vm.install()
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
            "Vox Scribe transcribes on the phone. It needs two models from the " +
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
private fun KeepScreenOn() {
    val view = LocalView.current
    DisposableEffect(Unit) {
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = false }
    }
}

@Composable
private fun Dictate(vm: DictationViewModel) {
    val context = LocalContext.current
    val askMic = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        if (ok) vm.start() else Toast.makeText(context, "Microphone permission is needed", Toast.LENGTH_LONG).show()
    }
    val toggle = {
        when {
            vm.recording -> vm.stop()
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED -> vm.start()
            else -> askMic.launch(Manifest.permission.RECORD_AUDIO)
        }
    }
    if (vm.recording) KeepScreenOn()

    Column(Modifier.fillMaxSize()) {
        Transcript(vm, Modifier.weight(1f).fillMaxWidth())
        Surface(tonalElevation = 3.dp) {
            Column(
                Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                val status = when {
                    vm.finishing -> "Finishing…"
                    vm.recording -> "Listening"
                    else -> "Tap to dictate"
                }
                Text(status, style = MaterialTheme.typography.labelLarge)
                Spacer(Modifier.height(8.dp))
                LinearProgressIndicator(
                    progress = { if (vm.recording) (vm.level * 8f).coerceIn(0f, 1f) else 0f },
                    modifier = Modifier.fillMaxWidth(0.6f),
                )
                Spacer(Modifier.height(12.dp))
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    val idle = !vm.recording && !vm.finishing
                    val hasText = vm.paragraphs.isNotEmpty()
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        TextButton(onClick = { copy(context, vm.text()) }, enabled = hasText) { Text("Copy") }
                        TextButton(onClick = { share(context, vm.text()) }, enabled = hasText) { Text("Share") }
                    }
                    RecordButton(recording = vm.recording, enabled = !vm.finishing, onClick = toggle)
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        OutlinedButton(onClick = vm::breakParagraph, enabled = vm.recording) { Text("¶") }
                        TextButton(onClick = vm::clear, enabled = idle && hasText) { Text("Clear") }
                    }
                }
            }
        }
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
private fun Transcript(vm: DictationViewModel, modifier: Modifier) {
    val list = rememberLazyListState()
    val last = vm.paragraphs.lastOrNull()
    LaunchedEffect(vm.paragraphs.size, last?.text) {
        if (vm.paragraphs.isNotEmpty()) list.animateScrollToItem(vm.paragraphs.size - 1)
    }
    if (vm.paragraphs.isEmpty()) {
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
    SelectionContainer(modifier) {
        LazyColumn(
            state = list,
            modifier = Modifier.fillMaxSize(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            items(vm.paragraphs, key = { it.id }) { p ->
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
