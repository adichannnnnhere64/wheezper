package com.wheezper.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.google.protobuf.Timestamp
import com.wheezper.app.Connection
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

private val timeFormat = DateTimeFormatter.ofPattern("HH:mm:ss")
private val dateFormat = DateTimeFormatter.ofPattern("MMM d, HH:mm")

fun formatTime(timestamp: Timestamp?): String =
    timestamp?.let { format(it, timeFormat) } ?: "--:--:--"

fun formatDate(timestamp: Timestamp?): String =
    timestamp?.let { format(it, dateFormat) } ?: "—"

private fun format(timestamp: Timestamp, formatter: DateTimeFormatter): String? =
    runCatching {
        Instant.ofEpochSecond(timestamp.seconds, timestamp.nanos.toLong())
            .atZone(ZoneId.systemDefault())
            .format(formatter)
    }.getOrNull()

fun durationLabel(start: Timestamp?, end: Timestamp?): String {
    if (start == null || end == null) return "—"
    val seconds = (end.seconds - start.seconds).coerceAtLeast(0)
    val minutes = seconds / 60
    return if (minutes >= 60) "${minutes / 60}h ${minutes % 60}m" else "${minutes}m"
}

fun connectionLabel(connection: Connection): String = when (connection) {
    Connection.Idle -> "idle"
    Connection.Connecting -> "connecting"
    Connection.Live -> "live"
    Connection.Reconnecting -> "reconnecting"
    Connection.Offline -> "offline"
}

fun connectionColor(connection: Connection): Color = when (connection) {
    Connection.Live -> StatusColors.Ok
    Connection.Connecting, Connection.Reconnecting -> StatusColors.Warn
    Connection.Offline -> StatusColors.Bad
    Connection.Idle -> StatusColors.Idle
}

@Composable
fun StatusPill(connection: Connection) {
    val color = connectionColor(connection)
    Surface(
        color = color.copy(alpha = 0.14f),
        shape = RoundedCornerShape(20.dp),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Box(Modifier.size(7.dp).clip(CircleShape).background(color))
            Text(
                connectionLabel(connection),
                style = MaterialTheme.typography.labelSmall,
                color = color,
            )
        }
    }
}

@Composable
fun SectionHeader(title: String, trailing: String? = null) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        if (trailing != null) {
            Text(
                trailing,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
fun EmptyState(text: String) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text,
            modifier = Modifier.padding(16.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
fun Banner(
    text: String,
    isError: Boolean,
    onDismiss: () -> Unit,
) {
    val accent = if (isError) MaterialTheme.colorScheme.error else StatusColors.Ok
    Surface(
        color = accent.copy(alpha = 0.12f),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(start = 14.dp, end = 4.dp, top = 6.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text,
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            TextButton(onClick = onDismiss) { Text("OK", color = accent) }
        }
    }
}

