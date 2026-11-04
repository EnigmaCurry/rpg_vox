plugins {
    id("com.android.application")
}

android {
    namespace = "com.enigmacurry.vox_room_spike"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.enigmacurry.vox_room_spike"
        // Same floor as scribe's app.
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    // Nothing here is released, so the debug key is the only key.
    buildTypes {
        release { isMinifyEnabled = false }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    // Deliberately none. A WebView and a Service are both in the
    // platform, and the question this app asks is about the platform.
}
