@file:OptIn(ExperimentalMaterial3Api::class)

package com.wheezper.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.interaction.DragInteraction
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
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
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.wheezper.app.AiAnswer
import com.wheezper.app.Connection
import com.wheezper.app.MainViewModel
import com.wheezper.app.UiState
import com.wheezper.app.data.MeetingRepository
import com.wheezper.v1.Session
import com.wheezper.v1.TranscriptEvent

@Composable
fun SessionScreen(state: UiState, vm: MainViewModel) {
    val session = state.openSession ?: return
    val completed = MeetingRepository.isCompleted(session)
    var assistantOpen by rememberSaveable { mutableStateOf(false) }
    // The transcript is the point of this screen; everything above it is
    // chrome and collapses into one row so the notes get the height.
    var searchOpen by rememberSaveable { mutableStateOf(false) }
    var helpOpen by rememberSaveable { mutableStateOf(false) }
    var briefOpen by rememberSaveable { mutableStateOf(false) }
    var serverOpen by rememberSaveable { mutableStateOf(false) }

    val query = state.transcriptQuery.trim().lowercase()
    val visible = remember(state.events, query) {
        if (query.isEmpty()) state.events
        else state.events.filter {
            it.content.lowercase().contains(query) || it.speaker.lowercase().contains(query)
        }
    }
    val listState = rememberLazyListState()

    // Follow the live transcript. A new line or a new answer scrolls to the
    // *end of that block* — not to its top, which left a freshly generated
    // answer below the fold — and the answer keeps being followed while it
    // streams. The moment the user drags the list, following stops until they
    // drag back to the end, so reading an older answer is never interrupted.
    var follow by rememberSaveable { mutableStateOf(true) }
    LaunchedEffect(listState) {
        listState.interactionSource.interactions.collect { interaction ->
            when (interaction) {
                is DragInteraction.Start -> follow = false
                is DragInteraction.Stop, is DragInteraction.Cancel ->
                    follow = !listState.canScrollForward
            }
        }
    }
    val answerChars = state.answers.values.sumOf { it.text.length }
    LaunchedEffect(visible.size, state.answers.size) {
        if (follow && visible.isNotEmpty()) {
            listState.scrollToBlockEnd(visible.lastIndex, animate = true)
        }
    }
    LaunchedEffect(answerChars) {
        if (follow && visible.isNotEmpty()) {
            listState.scrollToBlockEnd(visible.lastIndex, animate = false)
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
            SessionChrome(
                connection = state.connection,
                server = state.settings.serverUrl,
                serverOpen = serverOpen,
                onServer = { serverOpen = !serverOpen },
                searchOpen = searchOpen,
                onSearch = {
                    // Collapsing drops the filter too: leaving a hidden query
                    // active would silently hide lines with no way to see it.
                    if (searchOpen) vm.setTranscriptQuery("")
                    searchOpen = !searchOpen
                },
                aiHelp = session.aiHelp && !completed,
                answers = state.answers.values,
                helpOpen = helpOpen,
                onHelp = { helpOpen = !helpOpen },
                hasBrief = session.aiContext.isNotBlank() || session.aiInstructions.isNotBlank(),
                briefOpen = briefOpen,
                onBrief = { briefOpen = !briefOpen },
            )

            if (searchOpen) {
                OutlinedTextField(
                    value = state.transcriptQuery,
                    onValueChange = vm::setTranscriptQuery,
                    placeholder = { Text("Search this transcript") },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 6.dp),
                )
            }

            if (session.aiHelp && !completed && helpOpen) {
                AiHelpPanel(state) { everyLine -> vm.setAssistantEveryLine(everyLine) }
            }

            if (briefOpen) {
                BriefPanel(session) { context, instructions ->
                    vm.saveGuidance(context, instructions)
                }
            }

            state.laptopCommand?.let { command ->
                CommandCard(command, onDismiss = vm::dismissLaptopCommand)
            }

            state.error?.let {
                Box(Modifier.padding(horizontal = 16.dp)) {
                    Banner(it, isError = true, onDismiss = vm::dismissError)
                }
            }

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
                        // One block per far-side line: the transcript row, and
                        // directly beneath it the answer to *that* row (if AI
                        // help answered it). Never a single shared answer slot —
                        // a fast interviewer would keep overwriting it.
                        TurnBlock(
                            event = event,
                            answer = state.answers[event.sequenceNumber],
                            onAsk = { vm.askAboutEvent(event) },
                        )
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
private fun TurnBlock(event: TranscriptEvent, answer: AiAnswer?, onAsk: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        // Double-tap asks about this line: the manual fallback when the
        // question heuristic saw a statement, and the way to get more than the
        // automatic one-liner. A single tap stays inert so scrolling and
        // reading are untouched.
        Box(
            modifier = Modifier.pointerInput(event.sequenceNumber, event.content) {
                detectTapGestures(onDoubleTap = { onAsk() })
            },
        ) {
            TranscriptRow(event)
        }
        if (answer != null) {
            // A rail down the left edge is what makes the pairing readable at
            // a glance: "them" row, then the suggestion that answers it.
            Row(
                modifier = Modifier
                    .padding(start = 14.dp)
                    .height(IntrinsicSize.Min),
            ) {
                Box(
                    Modifier
                        .width(2.dp)
                        .fillMaxHeight()
                        .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.45f))
                )
                AnswerCard(answer)
            }
        }
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
 * Everything above the transcript collapsed into one row: connection, search
 * and interview help. Each was a full-width block before, and three of them
 * cost most of the screen before a single line of notes was visible. Tap a
 * chip to open its panel; the transcript gets the height back.
 */
@Composable
private fun SessionChrome(
    connection: Connection,
    server: String,
    serverOpen: Boolean,
    onServer: () -> Unit,
    searchOpen: Boolean,
    onSearch: () -> Unit,
    aiHelp: Boolean,
    answers: Collection<AiAnswer>,
    helpOpen: Boolean,
    onHelp: () -> Unit,
    hasBrief: Boolean,
    briefOpen: Boolean,
    onBrief: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(Modifier.clickable(onClick = onServer)) { StatusPill(connection) }
        if (serverOpen) {
            Text(
                server.removePrefix("http://").removePrefix("https://"),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
        } else {
            Spacer(Modifier.weight(1f))
        }
        ChromeChip(
            // A dot marks a saved brief so an empty assistant answer is not
            // mistaken for "the model ignored the context I wrote".
            label = if (hasBrief) "Brief ·" else "Brief",
            active = briefOpen,
            onClick = onBrief,
        )
        if (aiHelp) {
            val thinking = answers.count { it.streaming }
            val label = when {
                thinking > 0 -> "AI · answering…"
                answers.isEmpty() -> "AI"
                answers.size == 1 -> "AI · 1"
                else -> "AI · ${answers.size}"
            }
            ChromeChip(label, active = helpOpen, onClick = onHelp)
        }
        ChromeChip("Search", active = searchOpen, onClick = onSearch)
    }
}

@Composable
private fun ChromeChip(label: String, active: Boolean, onClick: () -> Unit) {
    val accent = MaterialTheme.colorScheme.primary
    val color = if (active) accent else MaterialTheme.colorScheme.onSurfaceVariant
    Surface(
        color = if (active) accent.copy(alpha = 0.16f) else MaterialTheme.colorScheme.surface,
        shape = MaterialTheme.shapes.small,
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = color,
            maxLines = 1,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
        )
    }
}

/**
 * Details for interview help, shown only when the AI chip is open. The answers
 * themselves render under the transcript row they answer (see [AnswerCard]);
 * this panel never holds a single shared answer slot, which a fast interviewer
 * would keep overwriting.
 *
 * The mode is a toggle here, not only in Settings: "questions only" leaves the
 * lines that are not questions without an answer, and that is the first thing
 * anyone wants to change when they expected help under every far-side line.
 */
@Composable
private fun AiHelpPanel(state: UiState, onEveryLine: (Boolean) -> Unit) {
    val accent = MaterialTheme.colorScheme.primary
    val answers = state.answers.values
    val thinking = answers.count { it.streaming }
    val status = when {
        thinking > 0 -> "answering…"
        answers.isEmpty() -> "waiting for a line to answer"
        answers.size == 1 -> "1 answer below"
        else -> "${answers.size} answers below"
    }
    Surface(
        color = accent.copy(alpha = 0.08f),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
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
                Surface(
                    color = accent.copy(alpha = 0.16f),
                    shape = MaterialTheme.shapes.small,
                    modifier = Modifier.clickable {
                        onEveryLine(!state.settings.assistantEveryLine)
                    },
                ) {
                    Text(
                        if (state.settings.assistantEveryLine) {
                            "every far-side line"
                        } else {
                            "questions only"
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = accent,
                        modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                    )
                }
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
                    "Answers stay under their line. Double-tap a line to ask about it.",
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
 * Scroll so the *end* of the transcript block at [index] is on screen. Plain
 * `animateScrollToItem` puts a tall row at the top of the viewport, which is
 * exactly what hides a long answer below it — the reason a freshly answered
 * question looked like nothing had happened.
 */
private suspend fun LazyListState.scrollToBlockEnd(index: Int, animate: Boolean) {
    if (animate) animateScrollToItem(index) else scrollToItem(index)
    // Let the row lay out before asking where its end is.
    withFrameNanos { }
    val info = layoutInfo
    val block = info.visibleItemsInfo.firstOrNull { it.index == index } ?: return
    val overflow = (block.offset + block.size) -
        (info.viewportEndOffset - info.afterContentPadding)
    if (overflow <= 0) return
    if (animate) animateScrollBy(overflow.toFloat()) else scrollBy(overflow.toFloat())
}

/**
 * The meeting brief: what this session is about and how the assistant should
 * answer. Both are prompt text the server stores with the session, so every
 * question — auto-answered or from the sheet — carries them. Saved on demand,
 * because the useful brief is usually written after the first question.
 */
@Composable
private fun BriefPanel(session: Session, onSave: (String, String) -> Unit) {
    var context by rememberSaveable(session.aiContext, session.aiInstructions) {
        mutableStateOf(session.aiContext)
    }
    var instructions by rememberSaveable(session.aiContext, session.aiInstructions) {
        mutableStateOf(session.aiInstructions)
    }
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = context,
                onValueChange = { context = it },
                label = { Text("What is this about?") },
                placeholder = {
                    Text("Senior Java backend interview; the team runs Spring Boot and Kubernetes.")
                },
                minLines = 2,
                maxLines = 5,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = instructions,
                onValueChange = { instructions = it },
                label = { Text("Answer guidance") },
                placeholder = { Text("First person, one sentence. Correct my grammar.") },
                minLines = 2,
                maxLines = 4,
                modifier = Modifier.fillMaxWidth(),
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                TextButton(onClick = { onSave(context, instructions) }) { Text("Save brief") }
                Text(
                    "Applied to the next answer. The transcript is never changed.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
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
            .padding(start = 6.dp),
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
                Text("↳ AI HELP", style = MaterialTheme.typography.labelSmall, color = accent)
                Text(
                    if (answer.streaming) "answering…" else "not part of the transcript",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
                Spacer(Modifier.weight(1f))
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
