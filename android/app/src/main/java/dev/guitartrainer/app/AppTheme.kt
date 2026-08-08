package dev.guitartrainer.app

import android.content.Context
import androidx.compose.ui.graphics.Color
import org.json.JSONObject

/**
 * Design-element colors loaded from the bundled `assets/theme.json`, with a
 * hardcoded fallback so a missing/malformed asset never breaks rendering.
 * Mirrors `guitar_trainer_core::theme::Theme`'s schema (minus the TUI-only
 * `selection_bg`/`selection_fg`, which have no Compose analog).
 */
data class AppTheme(
    val accent: Color,
    val success: Color,
    val danger: Color,
    val secondary: Color,
) {
    companion object {
        val DEFAULT = AppTheme(
            accent = Color(0xFF6750A4),
            success = Color(0xFF4CAF50),
            danger = Color(0xFFE53935),
            secondary = Color(0xFF79747E),
        )
    }
}

private fun parseHex(s: String, fallback: Color): Color =
    try {
        Color(android.graphics.Color.parseColor(s))
    } catch (e: IllegalArgumentException) {
        fallback
    }

fun loadAppTheme(context: Context): AppTheme =
    try {
        val text = context.assets.open("theme.json").bufferedReader().use { it.readText() }
        val j = JSONObject(text)
        AppTheme(
            accent = parseHex(j.optString("accent"), AppTheme.DEFAULT.accent),
            success = parseHex(j.optString("success"), AppTheme.DEFAULT.success),
            danger = parseHex(j.optString("danger"), AppTheme.DEFAULT.danger),
            secondary = parseHex(j.optString("secondary"), AppTheme.DEFAULT.secondary),
        )
    } catch (e: Exception) {
        AppTheme.DEFAULT
    }
