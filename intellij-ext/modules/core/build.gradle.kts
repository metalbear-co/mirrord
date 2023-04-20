fun properties(key: String) = project.findProperty(key).toString()

plugins {
    // Java support
    id("java")
    // Kotlin support
    id("org.jetbrains.kotlin.jvm") version "1.8.10"
    id("org.jetbrains.intellij") version "1.+"

}

intellij {
    version.set(properties("platformVersion"))
}


dependencies {
    implementation("com.google.code.gson:gson:2.10")
    implementation("com.github.zafarkhaja:java-semver:0.9.0")
    implementation("org.jetbrains.kotlinx:kotlinx-collections-immutable:0.3.5")
}