package app.magnetar.ui

import android.net.Uri
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
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
import app.magnetar.OpenRequest
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.detail.DetailScreen
import app.magnetar.ui.downloads.DownloadsScreen
import app.magnetar.ui.saved.SavedScreen
import app.magnetar.ui.search.SearchScreen
import app.magnetar.ui.settings.SettingsScreen
import app.magnetar.ui.status.StatusScreen
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.filterNotNull

private enum class Tab(val route: String, val label: String, val icon: () -> ImageVector) {
    SEARCH("search", "Search", { Icons.Filled.Search }),
    SAVED("saved", "Saved", { Icons.Filled.Star }),
    DOWNLOADS("downloads", "Downloads", { AppIcons.Download }),
    STATUS("status", "Status", { Icons.Filled.Info }),
    SETTINGS("settings", "Settings", { Icons.Filled.Settings }),
}

private const val DETAIL = "detail/{infohash}?magnet={magnet}"

private fun detailRoute(infohash: String, magnet: String? = null) =
    "detail/$infohash" + (magnet?.let { "?magnet=" + Uri.encode(it) } ?: "")

@Composable
fun MagnetarNavHost(openRequests: MutableStateFlow<OpenRequest?>) {
    val nav = rememberNavController()
    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route
    val showBar = Tab.entries.any { it.route == route }

    LaunchedEffect(nav) {
        openRequests.filterNotNull().collect { r ->
            openRequests.value = null
            nav.navigate(detailRoute(r.infohash, r.magnet)) { launchSingleTop = true }
        }
    }

    Scaffold(
        contentWindowInsets = WindowInsets(0),
        bottomBar = {
            if (showBar) {
                NavigationBar {
                    for (tab in Tab.entries) {
                        NavigationBarItem(
                            selected = route == tab.route,
                            onClick = { nav.switchTab(tab.route) },
                            icon = { Icon(tab.icon(), null) },
                            label = { Text(tab.label) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        NavHost(
            navController = nav,
            startDestination = Tab.SEARCH.route,
            modifier = Modifier.padding(padding).consumeWindowInsets(padding),
        ) {
            composable(Tab.SEARCH.route) { SearchScreen(onOpen = { nav.navigate(detailRoute(it)) }) }
            composable(Tab.SAVED.route) { SavedScreen(onRun = { nav.switchTab(Tab.SEARCH.route) }) }
            composable(Tab.DOWNLOADS.route) { DownloadsScreen() }
            composable(Tab.STATUS.route) { StatusScreen() }
            composable(Tab.SETTINGS.route) { SettingsScreen() }
            composable(
                DETAIL,
                arguments = listOf(
                    navArgument("infohash") { type = NavType.StringType },
                    navArgument("magnet") {
                        type = NavType.StringType
                        nullable = true
                        defaultValue = null
                    },
                ),
            ) {
                DetailScreen(onBack = {
                    if (!nav.popBackStack()) nav.navigate(Tab.SEARCH.route)
                })
            }
        }
    }
}

private fun NavHostController.switchTab(route: String) = navigate(route) {
    popUpTo(graph.findStartDestination().id) { saveState = true }
    launchSingleTop = true
    restoreState = true
}
