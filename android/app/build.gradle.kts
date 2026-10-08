plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.enigmacurry.rpg_vox_scribe"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.enigmacurry.rpg_vox_scribe"
        // AAudio-era devices; matches the CLI build's API level.
        minSdk = 26
        targetSdk = 36
        // Minutes since 2020 unless given: every build, local or CI, is newer
        // than the ones before it, so any can install over any earlier one.
        versionCode = (findProperty("scribeVersionCode") as String?)?.toInt()
            ?: ((System.currentTimeMillis() - 1_577_836_800_000L) / 60_000).toInt()
        versionName = findProperty("scribeVersionName") as String? ?: "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    // A stable release key (SCRIBE_KEYSTORE etc., see `just android-apk`)
    // lets new APKs install over old ones; without it the debug key signs,
    // and a CI-built debug key differs every run.
    val keystore = System.getenv("SCRIBE_KEYSTORE")?.takeIf { it.isNotEmpty() }
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("SCRIBE_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("SCRIBE_KEY_ALIAS") ?: "scribe"
                keyPassword = System.getenv("SCRIBE_KEY_PASSWORD") ?: storePassword
            }
        }
    }

    buildTypes {
        // Same key as releases, so local builds and releases install over
        // each other without losing the app's models and threads.
        debug {
            if (keystore != null) signingConfig = signingConfigs.getByName("release")
        }
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName(if (keystore != null) "release" else "debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { compose = true }
}

/** CHANGELOG.md from the repository root, shipped as an asset for the About page. */
abstract class CopyChangelog : DefaultTask() {
    @get:InputFile abstract val source: RegularFileProperty
    @get:OutputDirectory abstract val out: DirectoryProperty

    @TaskAction
    fun copy() {
        source.get().asFile.copyTo(out.get().file("CHANGELOG.md").asFile, overwrite = true)
    }
}

val changelog = tasks.register<CopyChangelog>("copyChangelog") {
    source.set(rootProject.file("../CHANGELOG.md"))
}
androidComponents {
    onVariants { it.sources.assets?.addGeneratedSourceDirectory(changelog, CopyChangelog::out) }
}

dependencies {
    val bom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(bom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.activity:activity-compose:1.13.0")
}
