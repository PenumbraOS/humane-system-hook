import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.Sync

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val rustAbi = "arm64-v8a"
val rustTarget = "aarch64-linux-android"
val rustExecutableName = "humane-server"
val packagedRustLibraryName = "libpenumbra_server_android.so"
val rustProjectDir = rootProject.layout.projectDirectory.dir("server-rs")
val rustTargetBinary = rustProjectDir.file("target/$rustTarget/release/$rustExecutableName")
val generatedJniLibsDir = layout.buildDirectory.dir("generated/jniLibs/main")

val androidVersionName = project.findProperty("versionName") as String? ?: "1.0"

val buildRustServerAndroid by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds the Rust server for Android arm64."
    workingDir = rustProjectDir.asFile
    commandLine("cargo", "ndk", "-t", rustAbi, "build", "--release")
    environment("PENUMBRA_VERSION", androidVersionName)

    inputs.property("penumbraVersion", androidVersionName)
    inputs.files(
        fileTree(rustProjectDir.asFile) {
            exclude("target/**")
        }
    )
    outputs.file(rustTargetBinary)
}

val stageRustServerJniLibs by tasks.registering(Sync::class) {
    group = "build"
    description = "Stages the Rust server executable as a JNI lib."
    dependsOn(buildRustServerAndroid)

    into(generatedJniLibsDir)
    from(rustTargetBinary) {
        into(rustAbi)
        rename { packagedRustLibraryName }
    }
}

android {
    sourceSets {
        getByName("main") {
            jniLibs.setSrcDirs(listOf(generatedJniLibsDir))
        }
    }

    namespace = "com.penumbraos.server"
    compileSdk = 34

    buildFeatures {
        buildConfig = true
    }

    signingConfigs {
        create("release") {
            storeFile = rootProject.file("abxdroppedapk.keystore")
            storePassword = "abxdroppedapk"
            keyAlias = "abxdroppedapk"
            keyPassword = "abxdroppedapk"
        }
    }

    defaultConfig {
        applicationId = "com.penumbraos.server"
        minSdk = 31
        targetSdk = 32
        versionCode = (project.findProperty("versionCode") as String?)?.toIntOrNull() ?: 1
        versionName = androidVersionName

        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
            keepDebugSymbols += "**/libpenumbra_server_android.so"
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

tasks.named("preBuild") {
    dependsOn(stageRustServerJniLibs)
}

dependencies {
    implementation("org.jmdns:jmdns:3.6.3")
}
