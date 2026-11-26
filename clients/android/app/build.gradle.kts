// SPDX-License-Identifier: Apache-2.0
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

val skylatchVersionName: String = providers.gradleProperty("skylatchVersionName").getOrElse("0.3.0")
val skylatchVersionCode: Int = providers.gradleProperty("skylatchVersionCode").map(String::toInt).getOrElse(300)

val releaseKeystore: String? = System.getenv("SKYLATCH_ANDROID_KEYSTORE")?.takeIf { it.isNotBlank() }

android {
    namespace = "io.github.maxesisn.skylatch"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.maxesisn.skylatch"
        minSdk = 26
        targetSdk = 35
        versionCode = skylatchVersionCode
        versionName = skylatchVersionName
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = file(releaseKeystore)
                storePassword = System.getenv("SKYLATCH_ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("SKYLATCH_ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("SKYLATCH_ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.05.01")
    implementation(composeBom)
    implementation("androidx.core:core-ktx:1.16.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20260814")
}
