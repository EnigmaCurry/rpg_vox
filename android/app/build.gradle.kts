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
        // `just android-apk` passes the release tag and a growing code.
        versionCode = (findProperty("scribeVersionCode") as String?)?.toInt() ?: 1
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
                keyAlias = System.getenv("SCRIBE_KEY_ALIAS")
                keyPassword = System.getenv("SCRIBE_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
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

dependencies {
    val bom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(bom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.activity:activity-compose:1.13.0")
}
