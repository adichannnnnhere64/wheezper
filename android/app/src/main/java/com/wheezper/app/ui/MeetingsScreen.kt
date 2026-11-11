@file:OptIn(ExperimentalMaterial3Api::class)

package com.wheezper.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.wheezper.app.MainViewModel
import com.wheezper.app.UiState
import com.wheezper.app.data.MeetingRepository
import com.wheezper.app.data.Settings
import com.wheezper.v1.Session
import com.wheezper.v1.SessionStatus

@Composable
fun MeetingsScreen(
    state: UiState,
    vm: MainViewModel,
    onOpenHistory: () -> Unit,
) {
    var creating by rememberSaveable { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        if (state.settings.paired) vm.refresh()
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        ConnectionCard(state)

        if (!state.settings.paired) {
            PairCard(state, vm)
        } else {
            Button(
                onClick = { creating = true },
                modifier = Modifier.fillMaxWidth(),
                enabled = !state.busy,
            ) {
                Text("New meeting")
            }

            Spacer(Modifier.height(2.dp))
            SectionHeader("Active", trailing = "${state.activeSessions.size}")
            if (state.activeSessions.isEmpty()) {
                EmptyState(
                    "No live meeting. Create one here, then run the wheezper command " +
                        "on the laptop so its transcript appears on this phone."
                )
            } else {
                state.activeSessions.forEach { session ->
                    SessionCard(session) { vm.openSession(session) }
                }
            }

            if (state.history.isNotEmpty()) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    SectionHeader("Recent", trailing = "${state.history.size}")
                }
                state.history.take(4).forEach { session ->
                    SessionCard(session) { vm.openSession(session) }
                }
                TextButton(onClick = onOpenHistory) { Text("All meetings in History") }
            }
        }

        state.notice?.let { Banner(it, isError = false, onDismiss = vm::dismissNotice) }
        state.error?.let { Banner(it, isError = true, onDismiss = vm::dismissError) }

        if (state.busy) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.Center,
            ) {
                CircularProgressIndicator(modifier = Modifier.height(20.dp))
            }
        }
        Spacer(Modifier.height(12.dp))
    }

    if (creating) {
        NewMeetingDialog(
            settings = state.settings,
            onDismiss = { creating = false },
            onCreate = { title, aiHelp, model, token, cloud, everyLine, context, instructions ->
                creating = false
                vm.saveAssistantSettings(model, token, cloud, everyLine)
                vm.createSession(title, aiHelp, context, instructions)
            },
        )
    }
}

@Composable
private fun ConnectionCard(state: UiState) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text("Server", style = MaterialTheme.typography.titleMedium)
                StatusPill(
                    when {
                        !state.settings.paired -> com.wheezper.app.Connection.Idle
                        state.busy -> com.wheezper.app.Connection.Connecting
                        else -> com.wheezper.app.Connection.Idle
                    }
                )
            }
            Text(
                state.settings.serverUrl.ifBlank { "not configured" },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                if (state.settings.paired) {
                    "This phone is paired. Transcript text travels only between your laptop and this server."
                } else {
                    "Pair this phone with the pairing code printed by wheezper-server."
                },
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun PairCard(state: UiState, vm: MainViewModel) {
    var server by rememberSaveable(state.settings.serverUrl) {
        mutableStateOf(state.settings.serverUrl)
    }
    var code by rememberSaveable { mutableStateOf("") }
    var name by rememberSaveable(state.settings.deviceName) {
        mutableStateOf(state.settings.deviceName.ifBlank { "android" })
    }

    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text("Pair this phone", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(
                value = server,
                onValueChange = { server = it },
                label = { Text("Server address") },
                placeholder = { Text("https://meeting.example.com:50051") },
                singleLine = true,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    keyboardType = KeyboardType.Uri,
                    imeAction = ImeAction.Next,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = code,
                onValueChange = { code = it },
                label = { Text("Pairing code") },
                singleLine = true,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    imeAction = ImeAction.Next,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text("Device name") },
                singleLine = true,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    imeAction = ImeAction.Done,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                onClick = { vm.pair(server, code, name) },
                enabled = !state.busy && server.isNotBlank() && code.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Pair device")
            }
            Text(
                "No permanent token is stored in this app: pairing exchanges the server's " +
                    "code for a short-lived device token.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
fun SessionCard(session: Session, onClick: () -> Unit) {
    Card(
        onClick = onClick,
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    session.title.ifBlank { "Untitled meeting" },
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                if (session.aiHelp) {
                    Text(
                        "AI",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.padding(end = 8.dp),
                    )
                }
                SessionStatusLabel(session)
            }
            Text(
                "${session.transcriptCount} lines · ${formatDate(session.createdAt)} · " +
                    durationLabel(session.startedAt, session.endedAt),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (MeetingRepository.isCompleted(session)) {
                Text(
                    "ended",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun SessionStatusLabel(session: Session) {
    val (text, color) = when (session.status) {
        SessionStatus.SESSION_STATUS_ACTIVE -> "live" to StatusColors.Ok
        SessionStatus.SESSION_STATUS_COMPLETED -> "complete" to StatusColors.Idle
        else -> "created" to StatusColors.Warn
    }
    Text(text, style = MaterialTheme.typography.labelSmall, color = color)
}

@Composable
private fun NewMeetingDialog(
    settings: Settings,
    onDismiss: () -> Unit,
    onCreate: (
        title: String,
        aiHelp: Boolean,
        model: String,
        token: String,
        cloud: Boolean,
        everyLine: Boolean,
        aiContext: String,
        aiInstructions: String,
    ) -> Unit,
) {
    var title by rememberSaveable { mutableStateOf("") }
    var aiHelp by rememberSaveable { mutableStateOf(false) }
    var aiContext by rememberSaveable { mutableStateOf("") }
    var aiInstructions by rememberSaveable { mutableStateOf("") }
    var everyLine by rememberSaveable { mutableStateOf(settings.assistantEveryLine) }
    var model by rememberSaveable { mutableStateOf(settings.assistantModel) }
    var token by rememberSaveable { mutableStateOf(settings.assistantToken) }
    var cloud by rememberSaveable { mutableStateOf(settings.assistantCloud) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("New meeting") },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                OutlinedTextField(
                    value = title,
                    onValueChange = { title = it },
                    label = { Text("Title") },
                    placeholder = { Text("Deployment review") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )

                // Prompt context, not a transcript field: what the session is
                // about goes into every assistant prompt so the answers fit.
                OutlinedTextField(
                    value = aiContext,
                    onValueChange = { aiContext = it },
                    label = { Text("What is this about? (optional)") },
                    placeholder = {
                        Text("Senior Java backend interview; the team runs Spring Boot and Kubernetes.")
                    },
                    minLines = 2,
                    maxLines = 4,
                    modifier = Modifier.fillMaxWidth(),
                )

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { aiHelp = !aiHelp },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Checkbox(checked = aiHelp, onCheckedChange = { aiHelp = it })
                    Column {
                        Text("Enable AI help", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "Suggested answers to the interviewer's questions, as committed lines arrive.",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }

                if (aiHelp) {
                    OutlinedTextField(
                        value = aiInstructions,
                        onValueChange = { aiInstructions = it },
                        label = { Text("Answer guidance (optional)") },
                        placeholder = {
                            Text("First person, one sentence. Correct my grammar.")
                        },
                        minLines = 2,
                        maxLines = 4,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text("Answer", style = MaterialTheme.typography.labelMedium)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilterChip(
                            selected = !everyLine,
                            onClick = { everyLine = false },
                            label = { Text("Questions only") },
                        )
                        FilterChip(
                            selected = everyLine,
                            onClick = { everyLine = true },
                            label = { Text("Every far-side line") },
                        )
                    }
                    Text(
                        "Only committed lines trigger an answer (provisional words are not published), " +
                            "and the candidate's own lines are never answered.",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )

                    OutlinedTextField(
                        value = model,
                        onValueChange = { model = it },
                        label = { Text("Model") },
                        placeholder = { Text("gemma3:1b") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Row(
                        modifier = Modifier.horizontalScroll(rememberScrollState()),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        listOf("gemma3:1b", "deepseek-v4.1-flash", "llama3.1:8b", "qwen3:8b")
                            .forEach { preset ->
                                Surface(
                                    color = MaterialTheme.colorScheme.surfaceVariant,
                                    shape = MaterialTheme.shapes.small,
                                    modifier = Modifier.clickable { model = preset },
                                ) {
                                    Text(
                                        preset,
                                        style = MaterialTheme.typography.labelSmall,
                                        modifier = Modifier.padding(horizontal = 8.dp, vertical = 6.dp),
                                    )
                                }
                            }
                    }

                    OutlinedTextField(
                        value = token,
                        onValueChange = { token = it },
                        label = { Text("Ollama token (optional; required for Ollama Cloud)") },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { cloud = !cloud },
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Switch(checked = cloud, onCheckedChange = { cloud = it })
                        Spacer(Modifier.padding(horizontal = 6.dp))
                        Text("Use Ollama Cloud (ollama.com)", style = MaterialTheme.typography.bodyMedium)
                    }
                    Text(
                        if (cloud) {
                            "Transcript text is sent to ollama.com. The token stays on this phone, is sent " +
                                "with each request, and is never stored or logged by the server."
                        } else {
                            "Uses the server's configured Ollama. The optional token is sent per request and " +
                                "never stored or logged by the server."
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    Text(
                        "The server creates the session and returns a one-meeting token. " +
                            "You will get the wheezper command to paste on the laptop.",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    onCreate(
                        title,
                        aiHelp,
                        model,
                        token,
                        cloud,
                        everyLine,
                        aiContext,
                        aiInstructions,
                    )
                },
            ) {
                Text("Create")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}
