plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val includeFrida = providers.gradleProperty("includeFrida")
    .map { it.equals("true", ignoreCase = true) || it == "1" }
    .getOrElse(false)

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
        versionCode = (project.findProperty("versionCode") as String?)?.toIntOrNull() ?: 1
        versionName = project.findProperty("versionName") as String? ?: "1.0"

        // Only arm64 — the Humane AI Pin is arm64-v8a only
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    sourceSets {
        getByName("main") {
            if (includeFrida) {
                jniLibs.srcDir("frida")
            }
        }
    }

    packaging {
        jniLibs {
            // Native libs MUST be extracted to disk so we can System.load() by absolute path
            // from inside the target process (ironman).
            useLegacyPackaging = true

            // The MusicKit AAR's own libc++_shared.so is stripped out of the AAR
            // (hook/libs) so everything links against AliuHook's newer libc++
            // (its liblsplant.so needs symbols the older Apple libc++ lacks).
            // Prebuilt Apple SDK lib — don't let AGP strip it.
            keepDebugSymbols += "**/libappleMusicSDK.so"

            if (includeFrida) {
                // Prevent AGP from stripping Frida Gadget files:
                // - libfrida-gadget.so must not be stripped (breaks the binary)
                // - libfrida-gadget.config.so is a JSON config file disguised as .so —
                //   strip would corrupt/fail on it
                keepDebugSymbols += "**/libfrida-gadget.so"
                keepDebugSymbols += "**/libfrida-gadget.config.so"
            }
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

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    kotlinOptions {
        jvmTarget = "11"
    }

    lint {
        disable += "ExpiredTargetSdkVersion"
    }
}

dependencies {
    implementation("com.aliucord:Aliuhook:1.1.4")

    // Apple MusicKit Android SDK (vendored under hook/libs, git-excluded — Apple's
    // SDK, not ours to commit). mediaplayback ships arm64 libappleMusicSDK.so
    // (self-contained software FairPlay); musickitauth provides TokenProvider.
    implementation(files("libs/mediaplayback-release-1.1.1.aar"))
    implementation(files("libs/musickitauth-release-1.1.2.aar"))
}
