package app.magnetar.ui.common

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/** The few Material icons not in material-icons-core (paths from Material Icons, Apache 2.0). */
object AppIcons {
    val Insights by lazy {
        icon(
            "Insights",
            "M21,8c-1.45,0 -2.26,1.44 -1.93,2.51l-3.55,3.56c-0.3,-0.09 -0.74,-0.09 -1.04,0l-2.55,-2.55" +
                "C12.27,10.45 11.46,9 10,9c-1.45,0 -2.27,1.44 -1.93,2.52l-4.56,4.55C2.44,15.74 1,16.55 1,18" +
                "c0,1.1 0.9,2 2,2c1.45,0 2.26,-1.44 1.93,-2.51l4.55,-4.56c0.3,0.09 0.74,0.09 1.04,0l2.55,2.55" +
                "C12.73,16.55 13.54,18 15,18c1.45,0 2.27,-1.44 1.93,-2.52l3.56,-3.55C21.56,12.26 23,11.45 23,10" +
                "C23,8.9 22.1,8 21,8zM15,9l0.94,-2.07L18,6l-2.06,-0.93L15,3l-0.92,2.07L12,6l2.08,0.93z" +
                "M3.5,11L4,9l2,-0.5L4,8L3.5,6L3,8L1,8.5L3,9z",
        )
    }
    val Graph by lazy { icon("AccountTree", "M22,11V3h-7v3H9V3H2v8h7V8h2v10h4v3h7v-8h-7v3h-2V8h2v3z") }
    val Public by lazy {
        icon(
            "Public",
            "M12,2C6.48,2 2,6.48 2,12s4.48,10 10,10 10,-4.48 10,-10S17.52,2 12,2zM11,19.93c-3.95,-0.49 -7,-3.85 -7,-7.93" +
                " 0,-0.62 0.08,-1.21 0.21,-1.79L9,15v1c0,1.1 0.9,2 2,2v1.93zM17.9,17.39c-0.26,-0.81 -1,-1.39 -1.9,-1.39h-1v-3" +
                "c0,-0.55 -0.45,-1 -1,-1H8v-2h2c0.55,0 1,-0.45 1,-1V7h2c1.1,0 2,-0.9 2,-2v-0.41c2.93,1.19 5,4.06 5,7.41" +
                " 0,2.08 -0.8,3.97 -2.1,5.39z",
        )
    }
    val FilterList by lazy { icon("FilterList", "M10,18h4v-2h-4v2zM3,6v2h18V6H3zM6,13h12v-2H6v2z") }
    val Sort by lazy { icon("Sort", "M3,18h6v-2H3v2zM3,6v2h18V6H3zM3,13h12v-2H3v2z") }
    val ContentCopy by lazy {
        icon(
            "ContentCopy",
            "M16,1H4c-1.1,0 -2,0.9 -2,2v14h2V3h12V1zM19,5H8c-1.1,0 -2,0.9 -2,2v14c0,1.1 0.9,2 2,2h11" +
                "c1.1,0 2,-0.9 2,-2V7c0,-1.1 -0.9,-2 -2,-2zM19,21H8V7h11v14z",
        )
    }
    val Folder by lazy {
        icon("Folder", "M10,4H4c-1.1,0 -2,0.9 -2,2v12c0,1.1 0.9,2 2,2h16c1.1,0 2,-0.9 2,-2V8c0,-1.1 -0.9,-2 -2,-2h-8l-2,-2z")
    }
    val File by lazy {
        icon("File", "M14,2H6c-1.1,0 -2,0.9 -2,2v16c0,1.1 0.9,2 2,2h12c1.1,0 2,-0.9 2,-2V8l-6,-6zM13,9V3.5L18.5,9H13z")
    }

    private fun icon(name: String, path: String): ImageVector =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .addPath(pathData = addPathNodes(path), fill = SolidColor(Color.Black))
            .build()
}
