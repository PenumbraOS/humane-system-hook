pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
        maven { url = uri("https://maven.aliucord.com/releases") }
    }
}

rootProject.name = "humane-system-hook"
include(":hook")
include(":injector")
