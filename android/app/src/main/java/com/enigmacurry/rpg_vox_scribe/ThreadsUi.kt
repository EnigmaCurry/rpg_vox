package com.enigmacurry.rpg_vox_scribe

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/** Rename or delete requests raised from the drawer or the title, shown as dialogs. */
class ThreadDialogs {
    var renaming by mutableStateOf<ThreadInfo?>(null)
    var deleting by mutableStateOf<ThreadInfo?>(null)
}

/**
 * The thread list: New thread, then every thread (most recent first)
 * with its time and last line. Tap opens one; long-press offers Rename
 * and Delete. While recording only the open thread is reachable.
 */
@Composable
fun ThreadDrawer(dialogs: ThreadDialogs, close: () -> Unit) {
    val locked = Scribe.busy()
    ModalDrawerSheet {
        Text(
            "Threads",
            style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.padding(start = 24.dp, top = 24.dp, bottom = 8.dp),
        )
        if (locked) {
            Text(
                "Stop recording to switch threads",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(start = 24.dp, bottom = 8.dp),
            )
        }
        ThreadRow(
            title = "+ New thread",
            subtitle = null,
            selected = false,
            enabled = !locked,
            onClick = {
                Scribe.newThread()
                close()
            },
        )
        HorizontalDivider(Modifier.padding(vertical = 8.dp))
        LazyColumn {
            items(Scribe.threadList, key = { it.id }) { t ->
                val open = t.id == Scribe.thread.id
                var menu by remember { mutableStateOf(false) }
                Box {
                    ThreadRow(
                        title = t.title,
                        subtitle = t.last + if (t.preview.isNotBlank()) " · ${t.preview}" else "",
                        selected = open,
                        enabled = open || !locked,
                        onClick = {
                            Scribe.switchTo(t.id)
                            close()
                        },
                        onLongClick = { menu = true },
                    )
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Rename") }, onClick = {
                            menu = false
                            dialogs.renaming = t
                        })
                        DropdownMenuItem(
                            text = { Text("Delete") },
                            enabled = !(open && locked),
                            onClick = {
                                menu = false
                                dialogs.deleting = t
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
        Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
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

@Composable
fun ThreadDialogHost(dialogs: ThreadDialogs) {
    dialogs.renaming?.let { t ->
        // Whole title selected, so typing replaces it.
        var text by remember(t.id) { mutableStateOf(TextFieldValue(t.title, TextRange(0, t.title.length))) }
        AlertDialog(
            onDismissRequest = { dialogs.renaming = null },
            title = { Text("Rename thread") },
            text = { OutlinedTextField(value = text, onValueChange = { text = it }, singleLine = true) },
            confirmButton = {
                TextButton(
                    onClick = {
                        Scribe.rename(t.id, text.text)
                        dialogs.renaming = null
                    },
                    enabled = text.text.isNotBlank(),
                ) { Text("Rename") }
            },
            dismissButton = { TextButton(onClick = { dialogs.renaming = null }) { Text("Cancel") } },
        )
    }
    dialogs.deleting?.let { t ->
        AlertDialog(
            onDismissRequest = { dialogs.deleting = null },
            title = { Text("Delete thread?") },
            text = { Text("\"${t.title}\" and its ${t.count} paragraph(s) will be deleted. This can't be undone.") },
            confirmButton = {
                TextButton(onClick = {
                    Scribe.delete(t.id)
                    dialogs.deleting = null
                }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { dialogs.deleting = null }) { Text("Cancel") } },
        )
    }
}
