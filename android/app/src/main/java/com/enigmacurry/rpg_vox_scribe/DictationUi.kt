package com.enigmacurry.rpg_vox_scribe

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.gestures.awaitEachGesture
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
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.contextmenu.builder.item
import androidx.compose.foundation.text.contextmenu.modifier.appendTextContextMenuComponents
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat

/** The open dictation: its text, with the record button and the rest below. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun DictationScreen() {
    val context = LocalContext.current
    val begin = { Dictations.start()?.let { Toast.makeText(context, it, Toast.LENGTH_LONG).show() }; Unit }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            begin()
        } else {
            Toast.makeText(context, "Microphone permission is needed", Toast.LENGTH_LONG).show()
        }
    }
    val toggle: () -> Unit = {
        when {
            Dictations.listening -> Dictations.stop()
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED -> begin()
            else -> ask.launch(Manifest.permission.RECORD_AUDIO)
        }
    }
    if (Dictations.listening) KeepScreenOn()
    val typing = Dictations.typing
    // Closing the keyboard ends typing: back to dictating. Only once it
    // has been up, not while it's still opening.
    val ime = WindowInsets.isImeVisible
    var imeWasUp by remember { mutableStateOf(false) }
    LaunchedEffect(ime, typing != null) {
        when {
            typing == null -> imeWasUp = false
            ime -> imeWasUp = true
            imeWasUp -> {
                imeWasUp = false
                Dictations.dictateMode()
            }
        }
    }
    val hasText = if (typing != null) typing.text.isNotBlank() else Dictations.laid.toks.isNotEmpty()
    Column(Modifier.fillMaxSize().imePadding()) {
        if (typing != null) {
            Typing(typing, Modifier.weight(1f).fillMaxWidth())
        } else {
            Editor(Modifier.weight(1f).fillMaxWidth())
        }
        // Out of the way while the keyboard is up; closing it comes back to them.
        if (!ime) Surface(tonalElevation = 3.dp) {
            Column(
                Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    when {
                        Dictations.finishing -> "Finishing…"
                        Dictations.paused -> "Paused"
                        Dictations.listening -> "Listening · tap the text to move the cursor"
                        typing != null -> "Typing"
                        hasText -> "Tap to dictate at the cursor"
                        else -> "Tap to dictate"
                    },
                    style = MaterialTheme.typography.labelLarge,
                    maxLines = 1,
                )
                Spacer(Modifier.height(12.dp))
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        TextButton(onClick = { copyText(context, Dictations.text()) }, enabled = hasText) { Text("Copy") }
                        TextButton(onClick = { shareText(context, Dictations.text()) }, enabled = hasText) { Text("Share") }
                    }
                    RecordButton(recording = Dictations.listening, enabled = !Dictations.finishing, onClick = toggle)
                    Box(Modifier.width(156.dp), contentAlignment = Alignment.Center) {
                        Column(horizontalAlignment = Alignment.CenterHorizontally) {
                            if (Dictations.listening) {
                                OutlinedButton(onClick = Dictations::togglePause) {
                                    Text(if (Dictations.paused) "▶" else "❚❚")
                                }
                            }
                            val pad = androidx.compose.foundation.layout.PaddingValues(0.dp)
                            Row {
                                // A new paragraph at the cursor.
                                OutlinedButton(onClick = Dictations::paragraphBreak, enabled = hasText, contentPadding = pad, modifier = Modifier.width(48.dp)) { Text("¶") }
                                Spacer(Modifier.width(4.dp))
                                // The sentence before the cursor.
                                OutlinedButton(
                                    onClick = Dictations::deleteSentence,
                                    enabled = hasText,
                                    contentPadding = pad,
                                    modifier = Modifier.width(48.dp).semantics { contentDescription = "Delete the last sentence" },
                                ) { Text("⌫") }
                                Spacer(Modifier.width(4.dp))
                                // Type at the cursor; closing the keyboard comes back here.
                                OutlinedButton(
                                    onClick = Dictations::typeMode,
                                    contentPadding = pad,
                                    modifier = Modifier.width(48.dp).semantics { contentDescription = "Type" },
                                ) { Text("⌨️") }
                            }
                        }
                    }
                }
            }
        }
    }
}

private object DeleteItem

/**
 * Typing mode: the text in an ordinary field, for the phone's keyboard,
 * focused with the keyboard up.
 */
@Composable
private fun Typing(value: TextFieldValue, modifier: Modifier) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    var layout by remember { mutableStateOf<TextLayoutResult?>(null) }
    val requester = remember { BringIntoViewRequester() }
    // Keep the cursor in view as the keyboard comes up over it (the view
    // shrinking as it slides in), and as it moves.
    var height by remember { mutableStateOf(0) }
    LaunchedEffect(height, value.selection, layout) {
        val l = layout ?: return@LaunchedEffect
        val at = value.selection.end.coerceIn(0, l.layoutInput.text.length)
        val r = l.getCursorRect(at)
        requester.bringIntoView(Rect(r.left, r.top, r.right, r.bottom + r.height))
    }
    Box(modifier.onSizeChanged { height = it.height }.verticalScroll(rememberScrollState()).padding(16.dp)) {
        BasicTextField(
            value = value,
            onValueChange = Dictations::type,
            textStyle = MaterialTheme.typography.bodyLarge.scaled(FONT_SCALES[Scribe.fontStep])
                .copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
            onTextLayout = { layout = it },
            modifier = Modifier.fillMaxWidth().focusRequester(focus).bringIntoViewRequester(requester),
        )
    }
}

/**
 * The text, as one block: a tap moves the purple cursor, where new speech
 * goes, to the nearest gap between words; selected words (and the breaks
 * between paragraphs) can be deleted. To change words by hand, ⌨️ below
 * switches to typing.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Editor(modifier: Modifier) {
    val laid = Dictations.laid
    var selection by remember { mutableStateOf(TextRange.Zero) }
    // Held while selecting, so new words don't shift the text under it.
    var held by remember { mutableStateOf<Laid?>(null) }
    val shown = held ?: laid
    val current by rememberUpdatedState(laid)
    val faint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.45f)
    val purple = if (isSystemInDarkTheme()) Color(0xFFCE93D8) else Color(0xFF8E24AA)
    val text = buildAnnotatedString {
        append(shown.text)
        for ((r, t) in shown.toks) {
            if (t.partial) addStyle(SpanStyle(color = faint, fontStyle = FontStyle.Italic), r.first, r.last + 1)
        }
    }
    val sel = TextRange(selection.start.coerceIn(0, text.length), selection.end.coerceIn(0, text.length))
    var layout by remember { mutableStateOf<TextLayoutResult?>(null) }
    val scroll = rememberScrollState()
    val requester = remember { BringIntoViewRequester() }
    // Keep the cursor in view, until the reader scrolls away from it.
    var follow by remember { mutableStateOf(true) }
    LaunchedEffect(scroll) {
        scroll.interactionSource.interactions.collect {
            if (it is DragInteraction.Start) follow = false
        }
    }
    // Also as the view changes size: the keyboard closing, the controls coming back.
    var height by remember { mutableStateOf(0) }
    LaunchedEffect(shown, layout, follow, height) {
        val l = layout ?: return@LaunchedEffect
        if (!follow || held != null || shown.caret !in 0..l.layoutInput.text.length) return@LaunchedEffect
        val r = l.getCursorRect(shown.caret)
        // With a line's room below it.
        requester.bringIntoView(Rect(r.left, r.top, r.right, r.bottom + r.height))
    }
    fun done() {
        selection = TextRange.Zero
        held = null
    }
    Box(modifier.onSizeChanged { height = it.height }.verticalScroll(scroll).padding(16.dp)) {
        BasicTextField(
            value = TextFieldValue(text, sel),
            onValueChange = {
                selection = it.selection
                held = if (it.selection.collapsed) null else held ?: current
            },
            readOnly = true,
            textStyle = MaterialTheme.typography.bodyLarge.scaled(FONT_SCALES[Scribe.fontStep])
                .copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = SolidColor(Color.Transparent),
            onTextLayout = { layout = it },
            modifier = Modifier
                .fillMaxWidth()
                .bringIntoViewRequester(requester)
                .appendTextContextMenuComponents {
                    if (sel.collapsed) return@appendTextContextMenuComponents
                    val picked = shown.toks.filter { (r, t) -> t.placed && r.first < sel.max && r.last >= sel.min }.map { it.second }
                    if (picked.isEmpty()) return@appendTextContextMenuComponents
                    separator()
                    item(key = DeleteItem, label = "Delete") {
                        Dictations.delete(picked)
                        done()
                        close()
                    }
                }
                .drawWithContent {
                    drawContent()
                    val l = layout ?: return@drawWithContent
                    if (shown.caret !in 0..l.layoutInput.text.length) return@drawWithContent
                    val r = l.getCursorRect(shown.caret)
                    drawRect(purple, Offset(r.left, r.top), Size(2.dp.toPx(), r.height))
                }
                .pointerInput(Unit) {
                    // A quick tap, so a long-press stays text selection.
                    awaitEachGesture {
                        val up = awaitTap(PointerEventPass.Main) ?: return@awaitEachGesture
                        val l = layout ?: return@awaitEachGesture
                        follow = true
                        Dictations.moveCursor(l.getOffsetForPosition(up))
                    }
                },
        )
        if (laid.toks.isEmpty() && !Dictations.listening) {
            Text(
                "Dictate text you can keep editing: tap the text to move the cursor, " +
                    "where new speech goes, and select words to delete or edit them. " +
                    "¶ starts a new paragraph.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 32.dp),
            )
        }
    }
}

private fun copyText(context: android.content.Context, text: String) {
    val cm = context.getSystemService(ClipboardManager::class.java)
    cm.setPrimaryClip(ClipData.newPlainText("Dictation", text))
    // Android 13+ shows its own confirmation.
    if (Build.VERSION.SDK_INT < 33) Toast.makeText(context, "Copied", Toast.LENGTH_SHORT).show()
}

private fun shareText(context: android.content.Context, text: String) {
    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
    context.startActivity(Intent.createChooser(send, "Share dictation"))
}
