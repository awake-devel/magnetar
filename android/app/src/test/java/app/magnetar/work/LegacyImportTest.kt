package app.magnetar.work

import app.magnetar.api.ApiException
import app.magnetar.api.SavedSearch
import app.magnetar.api.SearchQuery
import app.magnetar.ui.FakeService
import kotlinx.coroutines.test.runTest
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class LegacyImportTest {
    private val ubuntu = """{"text":"ubuntu","categories":["application"],"limit":50,"offset":0}"""

    @Test
    fun `old searches are created once, the ones already there are skipped`() = runTest {
        val fake = FakeService()
        fake.saved += SavedSearch(id = 7, name = "Ubuntu", query = SearchQuery(text = "ubuntu", categories = listOf("application")))
        val old = listOf(
            LegacySearch("Ubuntu", ubuntu, notify = true),
            LegacySearch("Dune", """{"text":"dune","sort":"seeders","unknown_field":1}""", notify = false),
            LegacySearch("", "not json", notify = true),
        )
        assertTrue(LegacyImport.upload(fake, old))
        assertEquals(fake.saved.map { it.name }, listOf("Ubuntu", "Dune", "Saved search"))
        assertEquals(fake.saved[1].query.text, "dune")
        assertEquals(fake.saved[1].query.sort.name, "SEEDERS")
        assertFalse(fake.saved[1].notify)
        // Run again (the old file could not be deleted): nothing is created twice.
        assertTrue(LegacyImport.upload(fake, old))
        assertEquals(fake.saved.size, 3)
    }

    @Test
    fun `a failed upload keeps the old files`() = runTest {
        val fake = object : FakeService() {
            override suspend fun createSavedSearch(search: SavedSearch): SavedSearch =
                throw ApiException.Http(500, "internal", "boom")
        }
        assertFalse(LegacyImport.upload(fake, listOf(LegacySearch("Ubuntu", ubuntu, notify = false))))
        assertTrue(LegacyImport.upload(fake, emptyList()))
    }
}
