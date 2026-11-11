package com.wheezper.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.wheezper.app.BuildConfig
import com.wheezper.app.data.PlayUpdater
import com.wheezper.app.data.findActivity
import kotlinx.coroutines.launch

private sealed interface UpdateState {
    data object Idle : UpdateState
    data object Checking : UpdateState
    data object UpToDate : UpdateState
    data object NotFromPlay : UpdateState
    data class Available(val versionCode: Int) : UpdateState
}

/**
 * "Update" backed by Google Play's in-app update flow.
 *
 * Play is the installer and the signer, so this is the only self-service update
 * that can actually replace the app: it asks Play for the newest build and opens
 * Play's own update screen. A sideloaded/debug build has no Play record, and the
 * section says so instead of failing at install time.
 */
@Composable
fun UpdateSection() {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state by remember { mutableStateOf<UpdateState>(UpdateState.Idle) }
    var status by remember { mutableStateOf("") }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(
                "wheezper companion ${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})",
                style = MaterialTheme.typography.bodyMedium,
            )
            OutlinedButton(
                enabled = state != UpdateState.Checking,
                onClick = {
                    state = UpdateState.Checking
                    status = "asking Google Play…"
                    scope.launch {
                        state = when (val result = PlayUpdater.check(context)) {
                            is PlayUpdater.Status.Available -> {
                                status = "version ${result.versionCode} is available on Play"
                                UpdateState.Available(result.versionCode)
                            }
                            PlayUpdater.Status.UpToDate -> {
                                status = "you are on the latest version"
                                UpdateState.UpToDate
                            }
                            PlayUpdater.Status.NotFromPlay -> {
                                status = "this build did not come from Google Play"
                                UpdateState.NotFromPlay
                            }
                        }
                    }
                },
            ) { Text("Check for updates") }
        }

        Text(
            status.ifEmpty {
                "Updates are delivered by Google Play; Play also installs them automatically."
            },
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val current = state) {
            is UpdateState.Available -> Button(
                modifier = Modifier.fillMaxWidth(),
                onClick = {
                    status = "opening Google Play…"
                    scope.launch {
                        val started = PlayUpdater.startImmediate(context, context.findActivity())
                        if (!started) {
                            status = "Play could not start the update flow; open the Play Store app"
                        }
                    }
                },
            ) { Text("Update via Google Play") }

            UpdateState.NotFromPlay -> Text(
                "This build was installed outside Google Play, so Play cannot update it. " +
                    "Install the Play version to get updates, or sideload a newer APK.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )

            else -> Unit
        }

        Text(
            "Release builds come from Google Play, which holds the signing key. An APK the app " +
                "downloads itself is signed by a different key and would be rejected by the " +
                "installer, which is why the update button uses Play's in-app update flow.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
