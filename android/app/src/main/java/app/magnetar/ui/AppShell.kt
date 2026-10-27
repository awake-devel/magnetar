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
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import app.magnetar.MagnetarApp
import app.magnetar.api.Features
import app.magnetar.api.MagnetarApi
import app.magnetar.ui.graph.GraphScreen
import app.magnetar.ui.graph.GraphViewModel
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.detail.DetailScreen
import app.magnetar.ui.detail.DetailViewModel
import app.magnetar.ui.saved.SavedScreen
import app.magnetar.ui.saved.SavedViewModel
import app.magnetar.ui.search.SearchScreen
import app.magnetar.ui.search.SearchViewModel
import app.magnetar.ui.settings.SettingsScreen
import app.magnetar.ui.settings.SettingsViewModel
import app.magnetar.ui.stats.StatsScreen
import app.magnetar.ui.stats.StatsViewModel
import app.magnetar.ui.title.TitleScreen
import app.magnetar.ui.title.TitleViewModel
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
    const val TITLE = "title/{mediaId}"
    const val GRAPH = "graph?path={path}"
    fun torrent(infohash: String) = "torrent/$infohash"
    fun title(mediaId: Long) = "title/$mediaId"

    /** [path]: `wot`, `relays`, `media/{id}`, `swarm/{infohash}` (an argument, so the slash is kept). */
    fun graph(path: String) = "graph?path=" + android.net.Uri.encode(path)
}

@OptIn(ExperimentalComposeUiApi::class)
@Composable
fun AppShell(app: MagnetarApp, openTorrent: MutableStateFlow<String?>) {
    val nav = rememberNavController()
    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route
    val pending by openTorrent.collectAsState()
    val settings by app.settingsStore.settings.collectAsState()
    val repo = app.repository
    val health by repo.health.collectAsState()
    // The title page needs `media_releases` (D44): an older server would answer 404.
    val toTitle: (Long) -> Unit = { id -> nav.navigate(Routes.title(id)) }
    val openTitle = toTitle.takeIf { health?.has(Features.MEDIA_RELEASES) == true }
    // The graphs need `graphs` (D47).
    val toGraph: (String) -> Unit = { path -> nav.navigate(Routes.graph(path)) }
    val openGraph = toGraph.takeIf { health?.has(Features.GRAPHS) == true }

    // Activity-scoped: the search, its filters and its scroll survive tab switches, and the
    // title page reads the active filters from it.
    val searchVm: SearchViewModel = viewModel(
        factory = viewModelFactory { initializer { SearchViewModel(repo, onTiming = app::logTiming) } },
    )

    LaunchedEffect(pending) {
        pending?.let {
            nav.navigate(Routes.torrent(it))
            openTorrent.value = null
        }
    }

    Scaffold(
        // Compose test tags become resource ids for the emulator UI test (android/uitest).
        modifier = Modifier.semantics { testTagsAsResourceId = true },
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
                            modifier = Modifier.testTag("tab_${tab.route}"),
                        )
                    }
                }
            }
        },
    ) { padding ->
        NavHost(nav, startDestination = Tab.SEARCH.route, modifier = Modifier.padding(padding)) {
            composable(Tab.SEARCH.route) {
                SearchScreen(
                    vm = searchVm,
                    threshold = settings.trustThreshold,
                    onOpen = { nav.navigate(Routes.torrent(it)) },
                    onOpenMedia = openTitle,
                    onOpenSettings = { nav.switchTab(Tab.SETTINGS.route) },
                )
            }
            composable(Tab.SAVED.route) {
                val vm: SavedViewModel = viewModel(factory = viewModelFactory { initializer { SavedViewModel(repo) } })
                SavedScreen(
                    vm = vm,
                    onRun = { nav.switchTab(Tab.SEARCH.route) },
                    onOpenSettings = { nav.switchTab(Tab.SETTINGS.route) },
                    notificationsOn = settings.notifySavedSearches,
                )
            }
            composable(Tab.STATS.route) {
                val vm: StatsViewModel = viewModel(factory = viewModelFactory { initializer { StatsViewModel(repo) } })
                StatsScreen(
                    vm = vm,
                    onSearch = { nav.switchTab(Tab.SEARCH.route) },
                    onOpenGraph = toGraph,
                    onOpenSettings = { nav.switchTab(Tab.SETTINGS.route) },
                )
            }
            composable(Tab.SETTINGS.route) {
                val vm: SettingsViewModel = viewModel(
                    factory = viewModelFactory {
                        initializer {
                            SettingsViewModel(
                                repo = repo,
                                update = app.settingsStore::update,
                                serviceFor = { s -> MagnetarApi.create(s.serverUrl, s.apiToken, app.http) },
                            )
                        }
                    },
                )
                SettingsScreen(vm = vm, settings = settings, health = health)
            }
            composable(
                Routes.GRAPH,
                arguments = listOf(navArgument("path") { type = NavType.StringType; defaultValue = "wot" }),
            ) { entry ->
                val path = entry.arguments?.getString("path").orEmpty()
                val vm: GraphViewModel = viewModel(
                    viewModelStoreOwner = entry,
                    factory = viewModelFactory { initializer { GraphViewModel(repo, path) } },
                )
                GraphScreen(
                    vm = vm,
                    onBack = { nav.popBackStack() },
                    onOpenTorrent = { nav.navigate(Routes.torrent(it)) },
                    onOpenMedia = toTitle,
                )
            }
            composable(Routes.TORRENT, arguments = listOf(navArgument("infohash") { type = NavType.StringType })) { entry ->
                val infohash = entry.arguments?.getString("infohash").orEmpty()
                val vm: DetailViewModel = viewModel(
                    viewModelStoreOwner = entry,
                    factory = viewModelFactory { initializer { DetailViewModel(repo, infohash) } },
                )
                DetailScreen(
                    vm = vm,
                    threshold = settings.trustThreshold,
                    onBack = { nav.popBackStack() },
                    onOpenMedia = openTitle,
                    onOpenGraph = openGraph,
                )
            }
            composable(Routes.TITLE, arguments = listOf(navArgument("mediaId") { type = NavType.LongType })) { entry ->
                val mediaId = entry.arguments?.getLong("mediaId") ?: 0L
                val vm: TitleViewModel = viewModel(
                    viewModelStoreOwner = entry,
                    factory = viewModelFactory { initializer { TitleViewModel(repo, mediaId, searchVm.state.value.filters) } },
                )
                TitleScreen(
                    vm = vm,
                    threshold = settings.trustThreshold,
                    onBack = { nav.popBackStack() },
                    onOpenTorrent = { nav.navigate(Routes.torrent(it)) },
                    onOpenGraph = openGraph,
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
