package com.enigmacurry.rpg_vox_scribe

import android.content.Intent
import android.net.Uri
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties

private const val SOURCE_URL = "https://github.com/EnigmaCurry/rpg_vox"

/** Full screen: the app's name, version and source, then its changelog. */
@Composable
fun About(close: () -> Unit) {
    Dialog(onDismissRequest = close, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        BackHandler(onBack = close)
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().systemBarsPadding()) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(4.dp)) {
                    IconButton(onClick = close) { Text("←", style = MaterialTheme.typography.titleLarge) }
                    Text("About", style = MaterialTheme.typography.titleLarge)
                }
                Column(
                    Modifier
                        .fillMaxSize()
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 20.dp, vertical = 8.dp),
                ) {
                    Header()
                    HorizontalDivider(Modifier.padding(vertical = 16.dp))
                    Changelog()
                }
            }
        }
    }
}

@Composable
private fun Header() {
    val context = LocalContext.current
    val info = remember {
        runCatching { context.packageManager.getPackageInfo(context.packageName, 0) }.getOrNull()
    }
    val code = info?.let {
        if (Build.VERSION.SDK_INT >= 28) it.longVersionCode else @Suppress("DEPRECATION") it.versionCode.toLong()
    }
    Text("Scribe", style = MaterialTheme.typography.headlineMedium)
    Text(
        "Version ${info?.versionName ?: "?"}" + (code?.let { " ($it)" } ?: ""),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(
        "Live transcription on your phone, with speech models that run on it.",
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.padding(top = 12.dp),
    )
    TextButton(
        onClick = {
            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(SOURCE_URL)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        },
        contentPadding = PaddingValues(0.dp),
    ) { Text("github.com/EnigmaCurry/rpg_vox") }
}

/** CHANGELOG.md (bundled from the repository), from its first release section on. */
@Composable
private fun Changelog() {
    val context = LocalContext.current
    val blocks = remember {
        runCatching { context.assets.open("CHANGELOG.md").bufferedReader().use { it.readText() } }
            .map(::parseChangelog)
            .getOrDefault(emptyList())
    }
    if (blocks.isEmpty()) {
        Text("No changelog in this build.", style = MaterialTheme.typography.bodyMedium)
        return
    }
    for (b in blocks) {
        when (b) {
            is Block.Heading -> Text(
                inline(b.text),
                style = if (b.level == 2) MaterialTheme.typography.titleLarge else MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(top = if (b.level == 2) 20.dp else 12.dp, bottom = 4.dp),
            )
            is Block.Bullet -> Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
                Text("•  ", style = MaterialTheme.typography.bodyMedium)
                Text(inline(b.text), style = MaterialTheme.typography.bodyMedium)
            }
            is Block.Paragraph -> Text(
                inline(b.text),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(vertical = 4.dp),
            )
        }
    }
}

private sealed interface Block {
    data class Heading(val level: Int, val text: String) : Block
    data class Bullet(val text: String) : Block
    data class Paragraph(val text: String) : Block
}

/**
 * The little Markdown CHANGELOG.md uses: `##`/`###` headings, `- `
 * bullets (continued on indented lines) and paragraphs. Everything before
 * the first `##` (the title and a note for maintainers) is left out, as
 * is a section with nothing in it.
 */
private fun parseChangelog(md: String): List<Block> {
    val out = mutableListOf<Block>()
    var started = false
    var open: StringBuilder? = null
    var bullet = false
    fun flush() {
        open?.let { out += if (bullet) Block.Bullet(it.toString()) else Block.Paragraph(it.toString()) }
        open = null
    }
    for (raw in md.lines()) {
        val line = raw.trimEnd()
        when {
            line.startsWith("## ") || line.startsWith("### ") -> {
                flush()
                started = true
                val level = if (line.startsWith("### ")) 3 else 2
                out += Block.Heading(level, line.substring(level + 1))
            }
            !started -> {}
            line.isBlank() -> flush()
            line.startsWith("- ") -> {
                flush()
                bullet = true
                open = StringBuilder(line.substring(2))
            }
            open != null -> open!!.append(' ').append(line.trim())
            else -> {
                bullet = false
                open = StringBuilder(line.trim())
            }
        }
    }
    flush()
    // Drop headings with nothing under them (an empty "Unreleased").
    return out.filterIndexed { i, b ->
        b !is Block.Heading || out.getOrNull(i + 1).let { n -> n != null && !(n is Block.Heading && n.level <= b.level) }
    }
}

/** **bold**, `code` and [text](link) (shown as its text). */
@Composable
private fun inline(text: String): AnnotatedString {
    val mono = SpanStyle(fontFamily = FontFamily.Monospace)
    return buildAnnotatedString {
        val token = Regex("""\*\*(.+?)\*\*|`(.+?)`|\[(.+?)]\((.+?)\)""")
        var at = 0
        for (m in token.findAll(text)) {
            append(text.substring(at, m.range.first))
            val (bold, code, link) = m.destructured
            when {
                bold.isNotEmpty() -> withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(bold) }
                code.isNotEmpty() -> withStyle(mono) { append(code) }
                else -> append(link)
            }
            at = m.range.last + 1
        }
        append(text.substring(at))
    }
}
