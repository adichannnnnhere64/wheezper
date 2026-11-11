package com.wheezper.app.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Flat, low-poly palette: no gradients, two speaker accents, one mint primary.
 * Everything is legible on a dark screen and cheap to render.
 */
private val DarkScheme = darkColorScheme(
    primary = Color(0xFF8FD1B5),
    onPrimary = Color(0xFF0A1F16),
    primaryContainer = Color(0xFF1E3A2E),
    onPrimaryContainer = Color(0xFFCDEBDC),
    secondary = Color(0xFF8FB6DC),
    onSecondary = Color(0xFF0C1C2A),
    tertiary = Color(0xFFE8B77A),
    background = Color(0xFF12141A),
    onBackground = Color(0xFFE8EAF0),
    surface = Color(0xFF191C24),
    onSurface = Color(0xFFE8EAF0),
    surfaceVariant = Color(0xFF232833),
    onSurfaceVariant = Color(0xFFAEB6C6),
    outline = Color(0xFF3A4150),
    error = Color(0xFFE88A8A),
    onError = Color(0xFF2A0F0F),
    errorContainer = Color(0xFF3A1E1E),
)

object SpeakerColors {
    val Me = Color(0xFF8FD1B5)
    val Them = Color(0xFF8FB6DC)
    val Other = Color(0xFFB9AFCF)

    fun of(speaker: String): Color = when (speaker.trim().lowercase()) {
        "me" -> Me
        "them" -> Them
        else -> Other
    }

    fun label(speaker: String): String = when (speaker.trim().lowercase()) {
        "me" -> "me"
        "them" -> "them"
        "" -> "single source"
        else -> speaker
    }
}

object StatusColors {
    val Ok = Color(0xFF8FD1B5)
    val Warn = Color(0xFFE8B77A)
    val Bad = Color(0xFFE88A8A)
    val Idle = Color(0xFF7C8496)
}

private val WzTypography = Typography(
    headlineSmall = TextStyle(fontSize = 22.sp, fontWeight = FontWeight.SemiBold, lineHeight = 28.sp),
    titleLarge = TextStyle(fontSize = 18.sp, fontWeight = FontWeight.SemiBold, lineHeight = 24.sp),
    titleMedium = TextStyle(fontSize = 15.sp, fontWeight = FontWeight.Medium, lineHeight = 20.sp),
    bodyLarge = TextStyle(fontSize = 15.sp, lineHeight = 22.sp),
    bodyMedium = TextStyle(fontSize = 14.sp, lineHeight = 20.sp),
    labelLarge = TextStyle(fontSize = 13.sp, fontWeight = FontWeight.Medium),
    labelMedium = TextStyle(fontSize = 12.sp, fontWeight = FontWeight.Medium),
    labelSmall = TextStyle(fontSize = 11.sp, fontWeight = FontWeight.Medium),
)

@Composable
fun WheezperTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = DarkScheme,
        typography = WzTypography,
        shapes = Shapes(
            extraSmall = RoundedCornerShape(6.dp),
            small = RoundedCornerShape(8.dp),
            medium = RoundedCornerShape(12.dp),
            large = RoundedCornerShape(16.dp),
        ),
        content = content,
    )
}
