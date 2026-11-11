@file:OptIn(ExperimentalMaterial3Api::class)

package com.wheezper.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.wheezper.app.AiAnswer
import com.wheezper.app.MainViewModel
import com.wheezper.app.UiState
import com.wheezper.app.data.MeetingRepository
import com.wheezper.v1.TranscriptEvent

@Composable
fun SessionScreen(state: UiState, vm: MainViewModel) {
    val session = state.openSession ?: return
    val completed = MeetingRepository.isCompleted(session)
    var assistantOpen by rememberSaveable { mutableStateOf(false) }

    val query = state.transcriptQuery.trim().lowercase()
    val visible = remember(state.events, query) {
        if (query.isEmpty()) state.events
        else state.events.filter {
            it.content.lowercase().contains(query) || it.speaker.lowercase().contains(query)
        }
    }
    val listState = rememberLazyListState()

    LaunchedEffect(visible.size) {
        if (visible.isNotEmpty()) {
            listState.animateScrollToItem(visible.lastIndex)
        }
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                navigationIcon = {
                    TextButton(onClick = vm::closeSession) { Text("Back") }
                },
                title = {
                    Column {
                        Text(
                            session.title.ifBlank { "Untitled meeting" },
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(
                            "${session.transcriptCount} lines · " +
                                if (completed) "completed" else "listening",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                actions = {
                    if (!completed) {
                        TextButton(onClick = vm::endSession) { Text("End") }
                    }
                    TextButton(onClick = { assistantOpen = true }) { Text("Ask AI") }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            ConnectionStrip(state.connection, state.settings.serverUrl)

            if (session.aiHelp && !completed) {
                AiHelpStrip(state)
            }

            state.laptopCommand?.let { command ->
                CommandCard(command, onDismiss = vm::dismissLaptopCommand)
            }

            state.error?.let {
                Box(Modifier.padding(horizontal = 16.dp)) {
                    Banner(it, isError = true, onDismiss = vm::dismissError)
                }
            }

            OutlinedTextField(
                value = state.transcriptQuery,
                onValueChange = vm::setTranscriptQuery,
                placeholder = { Text("Search this transcript") },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )

            if (visible.isEmpty()) {
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth(),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        if (state.connection == com.wheezper.app.Connection.Offline) {
                            "Waiting for the server. The laptop keeps transcribing locally."
                        } else {
                            "Waiting for the laptop's first committed line…"
                        },
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(32.dp),
                    )
                }
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth(),
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 24.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(visible, key = { it.sequenceNumber }) { event ->
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            TranscriptRow(event)
                            state.answers[event.sequenceNumber]?.let { AnswerCard(it) }
                        }
                    }
                }
            }
        }
    }

    if (assistantOpen) {
        AssistantSheet(state, vm) { assistantOpen = false }
    }
}

@Composable
private fun TranscriptRow(event: TranscriptEvent) {
    val color = SpeakerColors.of(event.speaker)
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Box(
                    Modifier
                        .size(7.dp)
                        .clip(MaterialTheme.shapes.small)
                        .background(color)
                )
                Text(
                    SpeakerColors.label(event.speaker),
                    style = MaterialTheme.typography.labelMedium,
                    color = color,
                )
                Spacer(Modifier.weight(1f))
                Text(
                    formatTime(event.startedAt),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(event.content, style = MaterialTheme.typography.bodyLarge)
        }
    }
}

/**
 * Status strip for interview help. The answers themselves render under the
 * transcript row they answer (see [AnswerCard]); this strip never holds a
 * single shared answer slot, which a fast interviewer would keep overwriting.
 */
@Composable
private fun AiHelpStrip(state: UiState) {
    val accent = MaterialTheme.colorScheme.primary
    val answers = state.answers.values
    val thinking = answers.count { it.streaming }
    val status = when {
        thinking > 0 -> "answering…"
        answers.isEmpty() -> "waiting for the next question"
        answers.size == 1 -> "1 answer below"
        else -> "${answers.size} answers below"
    }
    Surface(
        color = accent.copy(alpha = 0.08f),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 6.dp),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(3.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text("AI HELP", style = MaterialTheme.typography.labelSmall, color = accent)
                Text(
                    if (state.settings.assistantEveryLine) "every far-side line" else "questions only",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.weight(1f))
                Text(
                    state.settings.assistantModel.ifBlank { "server default" },
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    "Answers stay under the question they belong to.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    status,
                    style = MaterialTheme.typography.labelSmall,
                    color = accent,
                    maxLines = 1,
                )
            }
        }
    }
}

/**
 * The assistant's answer to this exact transcript row. It stays here as the
 * meeting goes on: a later question gets its own card under its own row.
 */
@Composable
private fun AnswerCard(answer: AiAnswer) {
    val accent = MaterialTheme.colorScheme.primary
    Surface(
        color = accent.copy(alpha = 0.10f),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 12.dp),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    "AI SUGGESTION — NOT PART OF THE TRANSCRIPT",
                    style = MaterialTheme.typography.labelSmall,
                    color = accent,
                )
                Spacer(Modifier.weight(1f))
                if (answer.streaming) {
                    Text(
                        "thinking…",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            when {
                answer.error != null -> Text(
                    answer.error,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
                answer.text.isNotBlank() -> Text(
                    answer.text,
                    style = MaterialTheme.typography.bodyLarge,
                )
                answer.streaming -> Text(
                    "Reading the transcript…",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                else -> Text(
                    "No answer.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun CommandCard(command: String, onDismiss: () -> Unit) {
    val clipboard = LocalClipboardManager.current
    Surface(
        color = MaterialTheme.colorScheme.primaryContainer,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 6.dp),
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                "Start transcription on the laptop",
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onPrimaryContainer,
            )
            Text(
                command,
                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                color = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(
                        MaterialTheme.colorScheme.background.copy(alpha = 0.35f),
                        MaterialTheme.shapes.small,
                    )
                    .padding(8.dp),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                TextButton(onClick = { clipboard.setText(AnnotatedString(command)) }) {
                    Text("Copy")
                }
                TextButton(onClick = onDismiss) { Text("Hide") }
            }
        }
    }
}

@Composable
private fun AssistantSheet(
    state: UiState,
    vm: MainViewModel,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = MaterialTheme.colorScheme.surface,
        modifier = Modifier.fillMaxWidth(),
    ) {
        var question by rememberSaveable { mutableStateOf("") }
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 20.dp, end = 20.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Ask about this meeting", style = MaterialTheme.typography.titleLarge)
            Text(
                "The server answers from the stored transcript. Suggestions are shown here " +
                    "and are never inserted into the transcript.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            OutlinedTextField(
                value = question,
                onValueChange = { question = it },
                label = { Text("Question") },
                placeholder = { Text("What did they say about the deployment deadline?") },
                minLines = 2,
                modifier = Modifier.fillMaxWidth(),
            )

            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Button(
                    onClick = { vm.askAssistant(question) },
                    enabled = !state.assistantStreaming && question.isNotBlank(),
                ) {
                    Text(if (state.assistantStreaming) "Thinking…" else "Ask")
                }
                TextButton(onClick = {
                    vm.clearAssistant()
                    question = ""
                }) {
                    Text("Clear")
                }
                Spacer(Modifier.weight(1f))
                if (state.assistantStatus.isNotBlank()) {
                    Text(
                        state.assistantStatus,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                listOf(
                    "What action items were mentioned?",
                    "Summarize the last five minutes.",
                    "What questions did they ask?",
                ).forEach { suggestion ->
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        shape = MaterialTheme.shapes.small,
                        modifier = Modifier.clickable { question = suggestion },
                    ) {
                        Text(
                            suggestion,
                            style = MaterialTheme.typography.labelSmall,
                            modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
                        )
                    }
                }
            }

            if (state.assistantAnswer.isNotBlank()) {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.primaryContainer,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(
                        modifier = Modifier.padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        Text(
                            "AI SUGGESTION — NOT PART OF THE TRANSCRIPT",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                        Text(
                            state.assistantAnswer,
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onPrimaryContainer,
                        )
                    }
                }
            } else if (state.assistantStreaming) {
                Text(
                    "Retrieving meeting context…",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(4.dp))
        }
    }
}
