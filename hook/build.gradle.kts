plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.penumbraos.hook"
    compileSdk = 34

    signingConfigs {
        create("release") {
            storeFile = rootProject.file("abxdroppedapk.keystore")
            storePassword = "abxdroppedapk"
            keyAlias = "abxdroppedapk"
            keyPassword = "abxdroppedapk"
        }
    }

    defaultConfig {
        applicationId = "com.penumbraos.hook"
        minSdk = 31
        targetSdk = 32
        versionCode = 1
        versionName = "1.0"

        // Only arm64 — the Humane AI Pin is arm64-v8a only
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    packaging {
        jniLibs {
            // Native libs MUST be extracted to disk so we can System.load() by absolute path
            // from inside the target process (ironman).
            useLegacyPackaging = true

            // Prevent AGP from stripping Frida Gadget files:
            // - libfrida-gadget.so must not be stripped (breaks the binary)
            // - libfrida-gadget.config.so is a JSON config file disguised as .so —
            //   strip would corrupt/fail on it
            keepDebugSymbols += "**/libfrida-gadget.so"
            keepDebugSymbols += "**/libfrida-gadget.config.so"
        }
    }

    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("release")
        }
        getByName("debug") {
            signingConfig = signingConfigs.getByName("release")
        }
    }

    lint {
        // We target API 32 (Android 12L on the Humane AI Pin) and sideload — not going to Play Store
        disable += "ExpiredTargetSdkVersion"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    kotlinOptions {
        jvmTarget = "11"
    }
}

dependencies {
    implementation("com.aliucord:Aliuhook:1.1.4")
}
