plugins {
    id("com.android.application") version "9.4.1" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
    // kotlinx.serialization compiler plugin; same version as the Kotlin compiler (docs/APP.md §2).
    id("org.jetbrains.kotlin.plugin.serialization") version "2.4.21" apply false
}
