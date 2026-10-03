import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "app.tidedesk.viewer"
    compileSdk = 36
    ndkVersion = "27.3.13750724"

    defaultConfig {
        applicationId = "app.tidedesk.viewer"
        // Android 8: MediaCodec's asynchronous mode and low-latency decoding
        // hints are all there.
        minSdk = 26
        targetSdk = 36
        versionCode = 12
        versionName = "0.1.0-alpha.12"
        // Phones are ARM64; one library keeps the app small.
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
}

// The Rust side, built for the phone with cargo-ndk into jniLibs before the
// libraries are packaged.
val cargoBuild by tasks.registering(Exec::class) {
    val release = gradle.startParameter.taskNames.any { it.contains("Release") }
    workingDir = rootDir.parentFile
    environment("ANDROID_NDK_HOME", android.ndkDirectory.absolutePath)
    commandLine(
        listOf("cargo", "ndk", "-t", "arm64-v8a", "-o", "android/app/src/main/jniLibs", "build", "-p", "tidedesk-android")
            + if (release) listOf("--release") else emptyList()
    )
}
tasks.named("preBuild") { dependsOn(cargoBuild) }
