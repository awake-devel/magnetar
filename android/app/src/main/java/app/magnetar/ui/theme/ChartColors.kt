package app.magnetar.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

/**
 * Chart and graph colours (D47). The categorical slots are a validated colour-blind-safe order
 * (adjacent pairs ΔE ≥ 8 under CVD simulation, ≥ 15 normal vision, light and dark steps chosen
 * separately); they are used in this fixed order and never cycled: past the last slot values
 * fold into "Other". Bars of one field are magnitude, so they use one hue ([bar]). Status
 * colours are reserved for seeded / dead / unknown and always come with a text label.
 */
@Immutable
data class ChartColors(
    val categorical: List<Color>,
    val bar: Color,
    val barSelected: Color,
    val other: Color,
    val good: Color,
    val critical: Color,
    val unknown: Color,
    val grid: Color,
    val axis: Color,
    val ink: Color,
    val inkMuted: Color,
    val edge: Color,
)

private val Light = ChartColors(
    categorical = listOf(
        Color(0xFF2A78D6), Color(0xFFEB6834), Color(0xFF1BAF7A), Color(0xFFEDA100),
        Color(0xFFE87BA4), Color(0xFF008300), Color(0xFF6250D6), Color(0xFFE34948),
    ),
    bar = Color(0xFF86B6EF),
    barSelected = Color(0xFF1C5CAB),
    other = Color(0xFFC3C2B7),
    good = Color(0xFF0CA30C),
    critical = Color(0xFFD03B3B),
    unknown = Color(0xFF898781),
    grid = Color(0xFFE1E0D9),
    axis = Color(0xFFC3C2B7),
    ink = Color(0xFF0B0B0B),
    inkMuted = Color(0xFF52514E),
    edge = Color(0x660B0B0B),
)

private val Dark = ChartColors(
    categorical = listOf(
        Color(0xFF3987E5), Color(0xFFD95926), Color(0xFF199E70), Color(0xFFC98500),
        Color(0xFFD55181), Color(0xFF008300), Color(0xFF9085E9), Color(0xFFE66767),
    ),
    bar = Color(0xFF256ABF),
    barSelected = Color(0xFF86B6EF),
    other = Color(0xFF52514E),
    good = Color(0xFF0CA30C),
    critical = Color(0xFFD03B3B),
    unknown = Color(0xFF898781),
    grid = Color(0xFF2C2C2A),
    axis = Color(0xFF383835),
    ink = Color(0xFFF0EFEC),
    inkMuted = Color(0xFFC3C2B7),
    edge = Color(0x66F0EFEC),
)

@Composable
fun chartColors(dark: Boolean = isSystemInDarkTheme()): ChartColors = if (dark) Dark else Light

/**
 * Slot of [value] in a field with a fixed vocabulary ([order]), so a value keeps its colour
 * whatever the filters do to its rank; null = not in the first slots (drawn as "Other").
 */
fun ChartColors.slotOf(value: String, order: List<String>): Color? =
    order.indexOf(value).takeIf { it in categorical.indices }?.let { categorical[it] }
