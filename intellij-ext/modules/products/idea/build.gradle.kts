plugins {
    // Java support
    id("java")
    // Kotlin support
    id("org.jetbrains.kotlin.jvm") version "1.6.10"
}


intellij {
    version = jetbrains.version
    plugins = [
        jetbrains.goland
    ]
}

dependencies {
    implementation project(":mirrord-core")
}