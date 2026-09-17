pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        maven { url = uri(rootDir.resolve(".ci/m2")) }
        mavenLocal()
        google()
        mavenCentral()
        // Re-enabled 2026-09-17: the host is serving again (verified 200 for
        // Aliuhook 1.1.4). :hook cannot resolve its only dependency without it.
        maven { url = uri("https://maven.aliucord.com/releases") }
    }
}

rootProject.name = "humane-system-hook"
include(":hook")
include(":injector")
include(":server")
