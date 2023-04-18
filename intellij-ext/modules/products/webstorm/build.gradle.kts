fun properties(key: String) = project.findProperty(key).toString()

plugins {
    // Java support
    id("java")
    // Kotlin support
    id("org.jetbrains.kotlin.jvm") version "1.8.10"
    // Gradle IntelliJ Plugin
    id("org.jetbrains.intellij") version "1.13.3"

}

tasks {
    buildSearchableOptions {
        enabled = false
    }
}

intellij {
    type.set("IU")
//    version.set("192.7142.36")
    version.set(properties("platformVersion"))
    plugins.set(listOf("JavaScript", "NodeJS", "java"))
//    plugins.set(listOf("java", "gradle", "maven", "org.intellij.scala:2022.3.8"))
}

// TODO: something
//runIde {
//    ideDir.set("/Users/$USERNAME/Library/Application Support/JetBrains/Toolbox/apps/WebStorm/ch-0/231.8109.174/WebStorm.app/Contents")
//}

dependencies {
    implementation(project(":mirrord-core"))
}
