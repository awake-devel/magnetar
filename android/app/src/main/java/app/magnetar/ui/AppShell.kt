package app.magnetar.ui

import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import app.magnetar.MagnetarApp
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.PlaceholderScreen
import app.magnetar.ui.saved.SavedScreen
import app.magnetar.ui.search.SearchScreen
import app.magnetar.ui.settings.SettingsScreen
import app.magnetar.ui.stats.StatsScreen
import kotlinx.coroutines.flow.MutableStateFlow

/** The four tabs of the bottom bar (docs/APP.md §1). */
enum class Tab(val route: String, val label: String, val icon: () -> ImageVector) {
    SEARCH("search", "Search", { Icons.Filled.Search }),
    SAVED("saved", "Saved", { Icons.Filled.Star }),
    STATS("stats", "Stats", { AppIcons.Insights }),
    SETTINGS("settings", "Settings", { Icons.Filled.Settings }),
}

object Routes {
    const val TORRENT = "torrent/{infohash}"
    fun torrent(infohash: String) = "torrent/$infohash"
}

@Composable
fun AppShell(app: MagnetarApp, openTorrent: MutableStateFlow<String?>) {
    val nav = rememberNavController()
    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route
    val pending by openTorrent.collectAsState()

    LaunchedEffect(pending) {
        pending?.let {
            nav.navigate(Routes.torrent(it))
            openTorrent.value = null
        }
    }

    Scaffold(
        bottomBar = {
            if (Tab.entries.any { it.route == route }) {
                NavigationBar {
                    for (tab in Tab.entries) {
                        NavigationBarItem(
                            selected = route == tab.route,
                            onClick = { nav.switchTab(tab.route) },
                            // The label names the item for TalkBack.
                            icon = { Icon(tab.icon(), contentDescription = null) },
                            label = { Text(tab.label) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        NavHost(nav, startDestination = Tab.SEARCH.route, modifier = Modifier.padding(padding)) {
            composable(Tab.SEARCH.route) { SearchScreen() }
            composable(Tab.SAVED.route) { SavedScreen() }
            composable(Tab.STATS.route) { StatsScreen() }
            composable(Tab.SETTINGS.route) { SettingsScreen(app) }
            composable(
                Routes.TORRENT,
                arguments = listOf(navArgument("infohash") { type = NavType.StringType }),
            ) { entry ->
                PlaceholderScreen(
                    title = "Torrent",
                    text = "Detail of ${entry.arguments?.getString("infohash").orEmpty()} " +
                        "comes with the search stage.",
                )
            }
        }
    }
}

private fun NavHostController.switchTab(route: String) = navigate(route) {
    popUpTo(graph.findStartDestination().id) { saveState = true }
    launchSingleTop = true
    restoreState = true
}
