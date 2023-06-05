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
    version.set(properties("platformVersion"))
    type.set("RD")
}

dependencies {
    implementation(project(":mirrord-core"))
}
