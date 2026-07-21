// Offxy for JetBrains IDEs — native .docx editor over the shared docxwasm engine.
// Spec: ../docs/superpowers/specs/2026-07-21-offxy-jetbrains-design.md

import org.jetbrains.intellij.platform.gradle.TestFrameworkType

plugins {
    id("org.jetbrains.kotlin.jvm") version "2.4.10"
    id("org.jetbrains.intellij.platform") version "2.18.1"
}

group = "dev.yeroo"
version = "0.1.0"

kotlin {
    jvmToolchain(17)
}

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        create("IC", "2024.2.5")
        testFramework(TestFrameworkType.Platform)
    }
    // The plugin's only runtime dependency: a pure-JVM wasm runtime that executes
    // the same docxwasm.wasm artifact the VS Code extension ships.
    implementation("com.dylibso.chicory:runtime:1.4.0")
    // Runtime bytecode compiler (JIT to JVM bytecode) — interpreter fallback
    // exists, but the compiler is what makes per-keystroke latency viable.
    implementation("com.dylibso.chicory:compiler:1.4.0")

    testImplementation("junit:junit:4.13.2")
}

intellijPlatform {
    pluginConfiguration {
        id = "dev.yeroo.offxy"
        name = "Offxy"
        version = project.version.toString()
        ideaVersion {
            sinceBuild = "242"
            untilBuild = provider { null }
        }
    }
}

// ---- shared wasm artifact ---------------------------------------------------
// The engine is the cargo workspace's docxwasm crate, built for
// wasm32-unknown-unknown and copied into plugin resources. Gradle's up-to-date
// checks give staleness for free: the cargo build reruns only when the Rust
// sources changed, and is skipped entirely while the artifact is fresh.

val wasmArtifact = file("../target/wasm32-unknown-unknown/release/docxwasm.wasm")

val buildWasm by tasks.registering(Exec::class) {
    workingDir = file("..")
    val cargo = file(System.getProperty("user.home") + "/.cargo/bin/cargo.exe")
        .takeIf { it.exists() }?.absolutePath ?: "cargo"
    commandLine(cargo, "build", "-p", "docxwasm", "--target", "wasm32-unknown-unknown", "--release")
    inputs.dir("../docxwasm/src")
    inputs.dir("../docxcore/src")
    inputs.dir("../opccore/src")
    outputs.file(wasmArtifact)
}

tasks.processResources {
    // from(task) wires both the file and the task dependency; the artifact
    // lands at the resource root (/docxwasm.wasm) without touching the src tree.
    from(buildWasm)
}
