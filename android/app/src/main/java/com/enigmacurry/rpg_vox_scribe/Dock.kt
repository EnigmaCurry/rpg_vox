package com.enigmacurry.rpg_vox_scribe

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/**
 * The controls along the bottom of every screen (dictation, transcription,
 * playback): one row, four round buttons mirrored around a big one in the
 * middle, under a short status line. A slot left null keeps its space, so
 * nothing shifts as buttons come and go.
 */
@Composable
fun Dock(
    status: String,
    live: Boolean,
    l2: (@Composable () -> Unit)?,
    l1: (@Composable () -> Unit)?,
    center: @Composable () -> Unit,
    r1: (@Composable () -> Unit)?,
    r2: (@Composable () -> Unit)?,
    above: (@Composable () -> Unit)? = null,
) {
    Surface(tonalElevation = 3.dp) {
        Column(
            Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 16.dp, vertical = 10.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            above?.invoke()
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(bottom = 8.dp)) {
                if (live) {
                    Box(Modifier.padding(end = 6.dp).size(8.dp).background(RED, CircleShape))
                }
                Text(
                    status,
                    style = MaterialTheme.typography.labelLarge,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                for (slot in listOf(l2, l1)) Slot(slot)
                center()
                for (slot in listOf(r1, r2)) Slot(slot)
            }
        }
    }
}

@Composable
private fun Slot(content: (@Composable () -> Unit)?) {
    Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) { content?.invoke() }
}

/** A round button in the dock: [icon], described as [label] for TalkBack. */
@Composable
fun DockButton(icon: ImageVector, label: String, enabled: Boolean = true, onClick: () -> Unit) {
    FilledTonalIconButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.size(48.dp),
    ) { Icon(icon, contentDescription = label) }
}

private val RED = Color(0xFFD32F2F)
private val GREEN = Color(0xFF7FD1A0)

/**
 * The big button in the middle: record (a red dot) or stop (a white
 * square on red), or with [icon] something else (play, pause). The ring
 * around it shows [ring] (0 to 1): the mic's level, or progress.
 */
@Composable
fun BigButton(
    label: String,
    enabled: Boolean,
    onClick: () -> Unit,
    recording: Boolean = false,
    icon: ImageVector? = null,
    ring: Float = 0f,
) {
    val track = MaterialTheme.colorScheme.surfaceVariant
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(80.dp)
            .semantics {
                contentDescription = label
                role = Role.Button
            },
    ) {
        Canvas(Modifier.size(80.dp)) {
            val w = 4.dp.toPx()
            val inset = w / 2
            val arc = Size(size.width - w, size.height - w)
            drawArc(track, 0f, 360f, false, Offset(inset, inset), arc, style = Stroke(w))
            if (ring > 0f) {
                drawArc(GREEN, -90f, 360f * ring.coerceIn(0f, 1f), false, Offset(inset, inset), arc, style = Stroke(w, cap = StrokeCap.Round))
            }
        }
        val fill = when {
            !enabled -> track
            icon != null -> MaterialTheme.colorScheme.primary
            recording -> RED
            else -> track
        }
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(66.dp)
                .clip(CircleShape)
                .background(fill)
                .clickable(enabled = enabled, onClick = onClick),
        ) {
            when {
                icon != null -> Icon(icon, null, tint = MaterialTheme.colorScheme.onPrimary, modifier = Modifier.size(32.dp))
                recording -> Box(Modifier.size(22.dp).background(Color.White, androidx.compose.foundation.shape.RoundedCornerShape(4.dp)))
                else -> Box(Modifier.size(24.dp).background(if (enabled) RED else RED.copy(alpha = 0.4f), CircleShape))
            }
        }
    }
}

/** Material icons the dock uses (24×24 paths), drawn here rather than pulling in the whole icon set. */
object DockIcons {
    private fun icon(name: String, path: String) = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        .addPath(addPathNodes(path), fill = SolidColor(Color.Black))
        .build()

    val Paragraph = icon("paragraph", "M10 3h9v2h-2v16h-2V5h-2v16h-2v-8a5 5 0 0 1 0-10z")
    val Backspace = icon(
        "backspace",
        "M22 3H7c-.69 0-1.23.35-1.59.88L0 12l5.41 8.11c.36.53.9.89 1.59.89h15c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm-3 " +
            "12.59L17.59 17 14 13.41 10.41 17 9 15.59 12.59 12 9 8.41 10.41 7 14 10.59 17.59 7 19 8.41 15.41 12 19 15.59z",
    )
    val Keyboard = icon(
        "keyboard",
        "M20 5H4c-1.1 0-1.99.9-1.99 2L2 17c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm-9 3h2v2h-2V8zm0 3h2v2h-2v-2zM8 " +
            "8h2v2H8V8zm0 3h2v2H8v-2zm-1 2H5v-2h2v2zm0-3H5V8h2v2zm9 7H8v-2h8v2zm0-4h-2v-2h2v2zm0-3h-2V8h2v2zm3 3h-2v-2h2v2zm0-3h-2V8h2v2z",
    )
    val Copy = icon(
        "copy",
        "M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z",
    )
    val Pause = icon("pause", "M6 19h4V5H6v14zm8-14v14h4V5h-4z")
    val Play = icon("play", "M8 5v14l11-7z")
    val Stop = icon("stop", "M6 6h12v12H6z")
    val Back = icon("back", "M11 18V6l-8.5 6 8.5 6zm.5-6l8.5 6V6l-8.5 6z")
    val Ahead = icon("ahead", "M4 18l8.5-6L4 6v12zm9-12v12l8.5-6L13 6z")
    val Clear = icon("clear", "M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z")
    val Mic = icon(
        "mic",
        "M12 14c1.66 0 2.99-1.34 2.99-3L15 5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.3-3c0 3-2.54 5.1-5.3 " +
            "5.1S6.7 14 6.7 11H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c3.28-.48 6-3.3 6-6.72h-1.7z",
    )
    val Phone = icon(
        "phone",
        "M17 1.01L7 1c-1.1 0-2 .9-2 2v18c0 1.1.9 2 2 2h10c1.1 0 2-.9 2-2V3c0-1.1-.9-1.99-2-1.99zM17 19H7V5h10v14z",
    )
    val Both = icon(
        "both",
        "M7 1h7c1.1 0 2 .9 2 2v4h-2V5H7v14h4v2H7c-1.1 0-2-.9-2-2V3c0-1.1.9-2 2-2zm10 9c1.1 0 2 .9 2 2v3c0 1.1-.9 2-2 2s-2-.9-2-2v-3c0-1.1.9-2 " +
            "2-2zm-4 5h1.5c0 1.38 1.12 2.5 2.5 2.5s2.5-1.12 2.5-2.5H21c0 1.86-1.28 3.41-3 3.86V23h-2v-2.14c-1.72-.45-3-2-3-3.86z",
    )
    val More = icon(
        "more",
        "M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z",
    )
}
