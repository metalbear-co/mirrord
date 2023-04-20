fun properties(key: String) = project.findProperty(key).toString()

plugins {
    // Java support
    id("java")
    // Kotlin support
    id("org.jetbrains.kotlin.jvm") version "1.8.10"
    // Gradle IntelliJ Plugin
    id("org.jetbrains.intellij") version "1.+"

}

tasks {
    buildSearchableOptions {
        enabled = false
    }
}

intellij {
    type.set("IU")
    version.set("231.8109.175")
    plugins.set(listOf("NodeJS"))
}

dependencies {
    implementation(project(":mirrord-core"))
}
