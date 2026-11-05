plugins {
    id("com.android.application")
}

android {
    namespace = "com.enigmacurry.vox_room"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.enigmacurry.vox_room"
        // Android 11. Higher than scribe's 26 on purpose: a typed
        // foreground service wants 29, and reading the window insets
        // without a compatibility branch wants 30. Nothing here has to
        // run on a phone older than the audio rules it depends on.
        minSdk = 30
        targetSdk = 36
        // Minutes since 2020 unless given: every build, local or CI, is
        // newer than the ones before it, so any can install over any
        // earlier one. Same scheme as scribe's app.
        versionCode = (findProperty("roomVersionCode") as String?)?.toInt()
            ?: ((System.currentTimeMillis() - 1_577_836_800_000L) / 60_000).toInt()
        versionName = findProperty("roomVersionName") as String? ?: "0.1.0"
    }

    // A stable release key lets new APKs install over old ones; without
    // it the debug key signs, and a CI-built debug key differs every run.
    val keystore = System.getenv("VOX_ROOM_KEYSTORE")?.takeIf { it.isNotEmpty() }
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("VOX_ROOM_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("VOX_ROOM_KEY_ALIAS") ?: "vox_room"
                keyPassword = System.getenv("VOX_ROOM_KEY_PASSWORD") ?: storePassword
            }
        }
    }

    buildTypes {
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
}

dependencies {
    // Deliberately none. A WebView and a Service are both in the
    // platform, and everything this app is for is the platform's.
}
