package app.magnetar.ui.settings

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import app.magnetar.R

/**
 * TMDB attribution required by the TMDB API Terms of Use §3 (read 2026-10-25): the TMDB logo,
 * less prominent than Magnetar's own marks, plus the exact notice below. Posters and overviews
 * come from the user's own Magnetar server, which uses the TMDB API (D42).
 */
const val TMDB_NOTICE = "This product uses TMDB and the TMDB APIs but is not endorsed, " +
    "certified, or otherwise approved by TMDB."

@Composable
fun TmdbAttribution(modifier: Modifier = Modifier) {
    Column(modifier.padding(vertical = 8.dp)) {
        Image(
            painter = painterResource(R.drawable.tmdb_logo),
            contentDescription = "TMDB",
            modifier = Modifier.padding(bottom = 4.dp),
        )
        Text(TMDB_NOTICE, style = MaterialTheme.typography.labelSmall)
        Text(
            // IMDb's customary wording for its non-commercial datasets (the dataset page itself
            // only points to the licensing terms; wording not re-verified there, D42).
            "Information courtesy of IMDb (https://www.imdb.com). Used with permission.",
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(top = 4.dp),
        )
    }
}
