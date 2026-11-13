plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.pixelcallai.streamer"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.pixelcallai.streamer"
        minSdk = 29
        targetSdk = 35
        versionCode = 27
        versionName = "0.2.4"
        multiDexEnabled = true
        // Default agent URL: localhost, mapped to the PC with `adb reverse tcp:8765 tcp:8787`.
        // Users can override it in the app (Prefs.agentUrl) for Wi-Fi/LAN mode.
        buildConfigField("String", "BRIDGE_WS_URL", "\"ws://127.0.0.1:8765\"")
        buildConfigField("int", "SAMPLE_RATE", "16000")
        // The agent streams all audio over the WebSocket; the phone-local menu stays OFF.
        buildConfigField("boolean", "LOCAL_MENU_ENABLED", "false")
    }

    buildFeatures {
        buildConfig = true
    }

    // Optional fixed signing key (release builds in CI): set PCA_KEYSTORE_FILE,
    // PCA_KEYSTORE_PASSWORD, PCA_KEY_ALIAS and PCA_KEY_PASSWORD. Without them the
    // build uses this machine's Android debug key, as before.
    val keystoreFile = System.getenv("PCA_KEYSTORE_FILE").orEmpty()
    if (keystoreFile.isNotEmpty()) {
        signingConfigs {
            create("fixed") {
                storeFile = file(keystoreFile)
                storePassword = System.getenv("PCA_KEYSTORE_PASSWORD").orEmpty()
                keyAlias = System.getenv("PCA_KEY_ALIAS").orEmpty()
                keyPassword = System.getenv("PCA_KEY_PASSWORD").orEmpty()
            }
        }
    }

    buildTypes {
        // Minify even debug: keeps a single dex with the app entry points. As a priv-app
        // on Android 16, a secondary dex caused ClassNotFound for MainActivity.
        debug {
            if (keystoreFile.isNotEmpty()) {
                signingConfig = signingConfigs.getByName("fixed")
            }
            isMinifyEnabled = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
        release {
            isMinifyEnabled = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    compileOnly(project(":xposed-stub"))
    // Keep deps tiny so debug builds stay single-dex / load cleanly on Android 16.
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("org.java-websocket:Java-WebSocket:1.5.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
}
