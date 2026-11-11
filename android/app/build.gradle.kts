import com.google.protobuf.gradle.id

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("com.google.protobuf")
}

android {
    namespace = "com.wheezper.app"
    compileSdk = 36

    // Release signing comes from the environment so the released APK keeps one
    // stable key (an update only installs over a build signed with the same
    // key). CI exports these from repository secrets; a local build without
    // them simply has no `release` signing config.
    val keystore = System.getenv("WHEEZPER_ANDROID_KEYSTORE")
    val keystorePassword = System.getenv("WHEEZPER_ANDROID_KEYSTORE_PASSWORD")
    val keyAliasName = System.getenv("WHEEZPER_ANDROID_KEY_ALIAS")
    val keyPasswordValue = System.getenv("WHEEZPER_ANDROID_KEY_PASSWORD")
    val hasReleaseKey = keystore != null && keystorePassword != null && keyAliasName != null

    signingConfigs {
        if (hasReleaseKey) {
            create("release") {
                storeFile = file(keystore!!)
                storePassword = keystorePassword
                keyAlias = keyAliasName
                keyPassword = keyPasswordValue ?: keystorePassword
            }
        }
    }

    defaultConfig {
        applicationId = "com.wheezper.app"
        minSdk = 26
        targetSdk = 36
        versionCode = 3
        versionName = "0.2.0"
    }

    buildTypes {
        debug {
            // local dev servers usually run plaintext gRPC
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (hasReleaseKey) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        // the update check compares the manifest's version_code with this build
        buildConfig = true
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
            excludes += "/META-INF/INDEX.LIST"
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

// The .proto files are the shared contract at the repo root. The protobuf
// plugin only scans src/main/proto, so keep a generated mirror of that folder
// (gitignored) instead of duplicating the contract.
val syncProtos by tasks.registering(Sync::class) {
    from(file("../../proto"))
    into(layout.projectDirectory.dir("src/main/proto"))
}
tasks.matching { it.name.startsWith("generate") && it.name.endsWith("Proto") }
    .configureEach { dependsOn(syncProtos) }
tasks.named("preBuild") { dependsOn(syncProtos) }

protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:3.25.5"
    }
    plugins {
        // Android uses the generated Java message classes + Kotlin coroutine stubs.
        create("grpc") {
            artifact = "io.grpc:protoc-gen-grpc-java:1.72.0"
        }
        create("grpckt") {
            artifact = "io.grpc:protoc-gen-grpc-kotlin:1.4.1:jdk8@jar"
        }
    }
    generateProtoTasks {
        all().forEach { task ->
            task.plugins {
                create("grpc")
                create("grpckt")
            }
            task.builtins {
                create("java")
            }
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.12.01")
    implementation(composeBom)

    implementation("androidx.core:core-ktx:1.17.0")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.4")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("androidx.datastore:datastore-preferences:1.1.7")

    // gRPC: OkHttp transport keeps the APK lean and works on Android's JVM.
    implementation("io.grpc:grpc-okhttp:1.72.0")
    implementation("io.grpc:grpc-protobuf:1.72.0")
    implementation("io.grpc:grpc-stub:1.72.0")
    implementation("io.grpc:grpc-kotlin-stub:1.4.1")
    implementation("com.google.protobuf:protobuf-java:3.25.5")
    implementation("javax.annotation:javax.annotation-api:1.3.2")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")

    // In-app update button: asks Google Play for the newest build (Play is the
    // installer and signer, so it is the only in-app path that can replace it).
    implementation("com.google.android.play:app-update:2.1.0")

    // JVM unit tests: the gRPC client is pure JVM, so it can be exercised
    // against a live server without an emulator.
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
}
