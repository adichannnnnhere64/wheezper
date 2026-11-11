package com.wheezper.app

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.wheezper.app.data.MeetingRepository
import com.wheezper.app.data.Settings
import com.wheezper.app.data.SettingsStore
import com.wheezper.app.data.WheezperClient
import com.wheezper.v1.Session
import com.wheezper.v1.TranscriptEvent
import io.grpc.Status
import io.grpc.StatusException
import io.grpc.StatusRuntimeException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

enum class Connection { Idle, Connecting, Live, Reconnecting, Offline }

/** One AI-help answer, anchored to the transcript row it answers. */
data class AiAnswer(
    val sequence: Long,
    val question: String,
    val text: String = "",
    val streaming: Boolean = true,
    val error: String? = null,
)

data class UiState(
    val settings: Settings = Settings(),
    val connection: Connection = Connection.Idle,
    val activeSessions: List<Session> = emptyList(),
    val history: List<Session> = emptyList(),
    val openSession: Session? = null,
    val events: List<TranscriptEvent> = emptyList(),
    val lastSequence: Long = 0,
    val busy: Boolean = false,
    val error: String? = null,
    val notice: String? = null,
    /** Ready-to-paste wheezper command for the meeting just created. */
    val laptopCommand: String? = null,
    val assistantQuestion: String = "",
    val assistantAnswer: String = "",
    val assistantStatus: String = "",
    val assistantStreaming: Boolean = false,
    /**
     * Automatic interview help, keyed by the transcript row each question
     * answers. Answers stay bound to their row, so the interviewer's next
     * question can never overwrite an answer that is still streaming.
     */
    val answers: Map<Long, AiAnswer> = emptyMap(),
    val autoAnsweredSequence: Long = 0,
    val transcriptQuery: String = "",
    val historyQuery: String = "",
)

/**
 * One owner for the connection, the live transcript and the assistant.
 * Transcript rows are append-only here too: a reconnect back-fills from the
 * server instead of guessing, and sequence numbers are the dedupe key.
 */
class MainViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsStore = SettingsStore(application)
    private var repository: MeetingRepository? = null
    private var streamJob: Job? = null
    private var statusJob: Job? = null
    private var assistantJob: Job? = null
    private var autoJob: Job? = null
    /**
     * Questions waiting for the assistant, in arrival order. A bounded FIFO,
     * not "newest wins": each answer is anchored to its row, so dropping an
     * older question would leave that row unanswered for good.
     */
    private val autoQueue = ArrayDeque<TranscriptEvent>()

    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            settingsStore.flow.collect { settings ->
                val previous = _state.value.settings
                _state.update { it.copy(settings = settings) }
                if (settings.serverUrl != previous.serverUrl || settings.deviceToken != previous.deviceToken) {
                    rebuildRepository(settings)
                    if (settings.paired) refresh()
                    _state.value.openSession?.let { startStreaming(it.id) }
                }
            }
        }
    }

    private fun rebuildRepository(settings: Settings) {
        repository?.close()
        repository = if (settings.serverUrl.isBlank()) {
            null
        } else {
            MeetingRepository(
                WheezperClient(settings.serverUrl, settings.deviceToken.ifBlank { null })
            )
        }
    }

    fun saveServer(url: String) {
        viewModelScope.launch { settingsStore.saveServer(url) }
    }

    /** Exchange the operator's pairing code for a device token. */
    fun pair(serverUrl: String, pairingCode: String, deviceName: String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null, notice = null) }
            runCatching {
                MeetingRepository.pairDevice(serverUrl.trim(), pairingCode.trim(), deviceName)
            }.onSuccess { response ->
                settingsStore.saveDevice(serverUrl, response.deviceToken, deviceName)
                _state.update {
                    it.copy(
                        busy = false,
                        notice = "Device paired. The token expires in the server's configured TTL.",
                        error = null,
                    )
                }
            }.onFailure { error ->
                _state.update { it.copy(busy = false, error = friendly(error)) }
            }
        }
    }

    fun forgetDevice() {
        viewModelScope.launch {
            settingsStore.forgetDevice()
            closeSession()
            _state.update {
                it.copy(
                    activeSessions = emptyList(),
                    history = emptyList(),
                    notice = "Device pairing removed from this phone.",
                )
            }
        }
    }

    /**
     * "Test connection": actually talk to the server the user typed in, and say
     * what happened. It used to be `refresh()`, which returned silently when the
     * phone was not paired and gave no visible result when it was — a dead button.
     */
    fun testConnection(serverUrl: String) {
        val url = serverUrl.trim()
        if (url.isBlank()) {
            _state.update { it.copy(error = "Enter the server address first, e.g. https://host:50051") }
            return
        }
        val token = _state.value.settings.deviceToken
        if (token.isBlank()) {
            _state.update {
                it.copy(
                    error = "Not paired yet. On the server run `wheezper-server --print-pairing-code`, " +
                        "then enter that code above.",
                )
            }
            return
        }
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null, notice = null) }
            // Test the address as typed, not the saved one: pressing "Test
            // connection" right after editing the field must test the edit.
            runCatching {
                MeetingRepository(WheezperClient(url, token)).use { repo ->
                    repo.listSessions(activeOnly = false)
                }
            }.onSuccess { sessions ->
                val active = sessions.count { !MeetingRepository.isCompleted(it) }
                _state.update {
                    it.copy(
                        busy = false,
                        notice = "Connected to $url — ${sessions.size} meeting(s), $active active.",
                        error = null,
                    )
                }
            }.onFailure { error ->
                _state.update {
                    it.copy(busy = false, error = "Cannot reach $url: ${friendly(error)}")
                }
            }
        }
    }

    fun refresh() {
        val repo = repository ?: return
        if (_state.value.settings.deviceToken.isBlank()) return
        viewModelScope.launch {
            _state.update { it.copy(busy = true) }
            runCatching { repo.listSessions(activeOnly = false) }
                .onSuccess { sessions ->
                    _state.update {
                        it.copy(
                            busy = false,
                            activeSessions = sessions.filterNot(MeetingRepository::isCompleted),
                            history = sessions.filter(MeetingRepository::isCompleted),
                            error = null,
                        )
                    }
                }
                .onFailure { error ->
                    _state.update { it.copy(busy = false, error = friendly(error)) }
                }
        }
    }

    /** Persist the AI provider defaults; called from the creation dialog and Settings. */
    fun saveAssistantSettings(
        model: String,
        token: String,
        cloud: Boolean,
        everyLine: Boolean,
    ) {
        viewModelScope.launch {
            settingsStore.saveAssistant(model, token, cloud, everyLine)
        }
    }

    fun createSession(title: String, aiHelp: Boolean = false) {
        val repo = repository ?: return
        val settings = _state.value.settings
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null, notice = null) }
            runCatching {
                repo.createSession(title.trim().ifBlank { "Untitled meeting" }, aiHelp)
            }
                .onSuccess { response ->
                    val session = response.session
                    settingsStore.saveSessionToken(session.id, response.sessionToken)
                    val command = buildString {
                        append("wheezper --source dual")
                        append(" --session ").append(session.id)
                        append(" --server ").append(settings.serverUrl.trimEnd('/'))
                        append(" --session-token ").append(response.sessionToken)
                    }
                    _state.update {
                        it.copy(
                            busy = false,
                            notice = if (aiHelp) {
                                "Meeting created with AI help. Start wheezper on the laptop; the assistant answers the far side as lines arrive."
                            } else {
                                "Meeting created. Start wheezper on the laptop with the command below."
                            },
                            laptopCommand = command,
                        )
                    }
                    refresh()
                    openSession(session)
                }
                .onFailure { error ->
                    _state.update { it.copy(busy = false, error = friendly(error)) }
                }
        }
    }

    fun openSession(session: Session) {
        assistantJob?.cancel()
        autoJob?.cancel()
        autoQueue.clear()
        _state.update {
            it.copy(
                openSession = session,
                events = emptyList(),
                lastSequence = 0,
                connection = Connection.Connecting,
                error = null,
                assistantQuestion = "",
                assistantAnswer = "",
                assistantStatus = "",
                assistantStreaming = false,
                answers = emptyMap(),
                autoAnsweredSequence = 0,
                transcriptQuery = "",
            )
        }
        startStreaming(session.id)
        startStatusPolling(session.id)
    }

    /**
     * Re-render the answers the server already stored under their transcript
     * rows. Answers used to exist only in this map, so closing the app lost a
     * whole interview's help; live streaming still wins for a row that is
     * answering right now.
     */
    private fun loadAnswers(sessionId: String) {
        val repo = repository ?: return
        viewModelScope.launch {
            val stored = runCatching { repo.listAnswers(sessionId) }.getOrNull() ?: return@launch
            if (stored.isEmpty()) return@launch
            _state.update { current ->
                if (current.openSession?.id != sessionId) return@update current
                val fresh = stored
                    .filter { it.answeredSequence > 0 && current.answers[it.answeredSequence] == null }
                    .associate { row ->
                        row.answeredSequence to AiAnswer(
                            sequence = row.answeredSequence,
                            question = row.question,
                            text = row.answer,
                            streaming = false,
                        )
                    }
                if (fresh.isEmpty()) {
                    current
                } else {
                    current.copy(answers = current.answers + fresh)
                }
            }
        }
    }

    fun closeSession() {
        streamJob?.cancel()
        statusJob?.cancel()
        assistantJob?.cancel()
        autoJob?.cancel()
        streamJob = null
        statusJob = null
        assistantJob = null
        autoJob = null
        autoQueue.clear()
        _state.update {
            it.copy(
                openSession = null,
                events = emptyList(),
                lastSequence = 0,
                connection = Connection.Idle,
                laptopCommand = null,
                answers = emptyMap(),
                autoAnsweredSequence = 0,
                transcriptQuery = "",
            )
        }
    }

    fun endSession() {
        val repo = repository ?: return
        val session = _state.value.openSession ?: return
        viewModelScope.launch {
            runCatching { repo.endSession(session.id) }
                .onSuccess { ended ->
                    _state.update { it.copy(openSession = ended, notice = "Meeting ended.") }
                    streamJob?.cancel()
                    refresh()
                }
                .onFailure { error -> _state.update { it.copy(error = friendly(error)) } }
        }
    }

    fun setTranscriptQuery(query: String) {
        _state.update { it.copy(transcriptQuery = query) }
    }

    fun setHistoryQuery(query: String) {
        _state.update { it.copy(historyQuery = query) }
    }

    fun dismissError() {
        _state.update { it.copy(error = null) }
    }

    fun dismissNotice() {
        _state.update { it.copy(notice = null) }
    }

    fun dismissLaptopCommand() {
        _state.update { it.copy(laptopCommand = null) }
    }

    /** Ask the meeting assistant; the answer streams in separately from the transcript. */
    fun askAssistant(question: String) {
        val repo = repository ?: return
        val session = _state.value.openSession ?: return
        if (question.isBlank()) return
        assistantJob?.cancel()
        _state.update {
            it.copy(
                assistantQuestion = question,
                assistantAnswer = "",
                assistantStatus = "asking the assistant…",
                assistantStreaming = true,
                error = null,
            )
        }
        val settings = _state.value.settings
        assistantJob = viewModelScope.launch {
            val answer = StringBuilder()
            var provider = ""
            var model = ""
            try {
                repo.streamAnswer(
                    session.id,
                    question,
                    settings.assistantModel,
                    settings.assistantToken,
                    settings.assistantCloud,
                ).collect { chunk ->
                    if (chunk.provider.isNotBlank()) provider = chunk.provider
                    if (chunk.model.isNotBlank()) model = chunk.model
                    if (chunk.delta.isNotEmpty()) {
                        answer.append(chunk.delta)
                        _state.update {
                            it.copy(
                                assistantAnswer = answer.toString(),
                                assistantStatus = if (provider.isNotBlank()) "$provider / $model" else "generating…",
                            )
                        }
                    }
                    if (chunk.done) {
                        _state.update {
                            it.copy(
                                assistantStreaming = false,
                                assistantStatus = if (provider.isNotBlank()) "$provider / $model" else "",
                            )
                        }
                    }
                }
                _state.update { it.copy(assistantStreaming = false) }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                _state.update {
                    it.copy(
                        assistantStreaming = false,
                        assistantStatus = "",
                        error = friendly(error),
                    )
                }
            }
        }
    }

    fun clearAssistant() {
        assistantJob?.cancel()
        _state.update {
            it.copy(
                assistantQuestion = "",
                assistantAnswer = "",
                assistantStatus = "",
                assistantStreaming = false,
            )
        }
    }

    private fun startStreaming(sessionId: String) {
        streamJob?.cancel()
        streamJob = viewModelScope.launch {
            var failures = 0
            var seededAuto = false
            while (isActive) {
                val repo = repository ?: break
                _state.update {
                    it.copy(
                        connection = if (failures == 0) Connection.Connecting else Connection.Reconnecting
                    )
                }
                try {
                    // Durable catch-up first: the server is the source of truth
                    // after a sleep, a network drop or a sequence gap. The very
                    // first catch-up must not auto-answer a *finished* meeting's
                    // history, but joining a running interview late should still
                    // get its unanswered questions answered (the queue is bounded,
                    // so the newest few win).
                    val live = _state.value.openSession
                        ?.let { !MeetingRepository.isCompleted(it) } == true
                    catchUp(repo, sessionId, autoAnswer = seededAuto || live)
                    // Another device may have answered rows of this meeting
                    // while this phone was closed; those answers are persisted.
                    loadAnswers(sessionId)
                    if (!seededAuto) {
                        seededAuto = true
                        // Reconnects keep this watermark, so lines that arrived
                        // while offline are still answered.
                        _state.update {
                            it.copy(autoAnsweredSequence = maxOf(it.autoAnsweredSequence, it.lastSequence))
                        }
                    }
                    failures = 0
                    _state.update { it.copy(connection = Connection.Live) }

                    repo.streamTranscripts(sessionId, _state.value.lastSequence).collect { event ->
                        if (event.sequenceNumber > _state.value.lastSequence + 1) {
                            // Do not show a later line over a hole in the middle.
                            catchUp(repo, sessionId)
                        }
                        addEvents(listOf(event))
                    }

                    // The stream closed. If the meeting is over, stop; otherwise
                    // treat it as a reconnect and loop.
                    val session = repo.getSession(sessionId)
                    _state.update { it.copy(openSession = session) }
                    if (MeetingRepository.isCompleted(session)) {
                        catchUp(repo, sessionId)
                        _state.update { it.copy(connection = Connection.Idle) }
                        break
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    failures++
                    _state.update { it.copy(connection = Connection.Offline) }
                    delay(minOf(15_000L, 1_000L * (1 shl minOf(failures, 4))))
                }
            }
        }
    }

    /**
     * The transcript stream stays open while a meeting is live, so it cannot
     * tell us that the laptop ended the session. A slow poll closes that gap.
     */
    private fun startStatusPolling(sessionId: String) {
        statusJob?.cancel()
        statusJob = viewModelScope.launch {
            while (isActive) {
                delay(20_000)
                val repo = repository ?: continue
                runCatching { repo.getSession(sessionId) }.onSuccess { session ->
                    _state.update { it.copy(openSession = session) }
                    if (MeetingRepository.isCompleted(session)) {
                        streamJob?.cancel()
                        _state.update { it.copy(connection = Connection.Idle) }
                        break
                    }
                }
            }
        }
    }

    private suspend fun catchUp(repo: MeetingRepository, sessionId: String, autoAnswer: Boolean = true) {
        var from = _state.value.lastSequence
        while (true) {
            val page = repo.sync(sessionId, from)
            addEvents(page.eventsList, autoAnswer)
            val next = _state.value.lastSequence
            if (!page.hasMore || next <= from) break
            from = next
        }
    }

    private fun addEvents(events: List<TranscriptEvent>, autoAnswer: Boolean = true) {
        if (events.isEmpty()) return
        var fresh: List<TranscriptEvent> = emptyList()
        _state.update { current ->
            val seen = current.events.mapTo(HashSet()) { it.sequenceNumber }
            fresh = events.filter { seen.add(it.sequenceNumber) }
            if (fresh.isEmpty()) {
                current
            } else {
                current.copy(
                    events = current.events + fresh,
                    lastSequence = maxOf(current.lastSequence, fresh.maxOf { it.sequenceNumber }),
                )
            }
        }
        if (fresh.isNotEmpty() && autoAnswer) maybeAutoAnswer(fresh)
    }

    /**
     * Interview help: answer the far side as committed lines arrive. "Every
     * line" answers each far-side line; the default only answers questions.
     *
     * Every matching line is queued in order and its answer is stored under the
     * transcript row it answers, so a fast interviewer's next question can
     * never overwrite an answer that is still streaming. One request is in
     * flight at a time (a local model shares the CPU with transcription), but
     * the queue is bounded so a slow model cannot build an endless backlog.
     */
    private fun maybeAutoAnswer(events: List<TranscriptEvent>) {
        val current = _state.value
        val session = current.openSession ?: return
        if (!session.aiHelp) return
        // Reopening a finished interview must not start generating answers for
        // its old questions.
        if (MeetingRepository.isCompleted(session)) return

        val everyLine = current.settings.assistantEveryLine
        val fresh = events.filter { event ->
            event.sequenceNumber > current.autoAnsweredSequence && wantsAnswer(event, everyLine)
        }
        if (fresh.isEmpty()) return

        fresh.forEach(autoQueue::addLast)
        while (autoQueue.size > AUTO_QUEUE_LIMIT) autoQueue.removeFirst()
        _state.update {
            it.copy(
                autoAnsweredSequence = maxOf(
                    it.autoAnsweredSequence,
                    fresh.maxOf { event -> event.sequenceNumber },
                )
            )
        }
        drainAutoQueue()
    }

    private fun drainAutoQueue() {
        if (autoJob?.isActive == true) return
        val event = autoQueue.removeFirstOrNull() ?: return
        startAutoAnswer(event)
    }

    private fun wantsAnswer(event: TranscriptEvent, everyLine: Boolean): Boolean {
        // The candidate's own lines are not questions to answer.
        if (event.speaker.equals("me", ignoreCase = true)) return false
        return if (everyLine) true else looksLikeQuestion(event.content)
    }

    private fun looksLikeQuestion(text: String): Boolean {
        val normalized = text.trim().lowercase()
        if (normalized.isEmpty()) return false
        if ('?' in normalized) return true
        return QUESTION_STARTERS.any { normalized.startsWith(it) }
    }

    private fun startAutoAnswer(event: TranscriptEvent) {
        val repo = repository
        val session = _state.value.openSession
        if (repo == null || session == null) {
            // Nothing to talk to: do not leave the queue stuck (drainAutoQueue
            // already removed this event).
            autoQueue.clear()
            return
        }
        val settings = _state.value.settings
        val sequence = event.sequenceNumber

        if (settings.assistantCloud && settings.assistantToken.isBlank()) {
            putAnswer(
                AiAnswer(
                    sequence = sequence,
                    question = event.content,
                    streaming = false,
                    error = "AI help is on, but Ollama Cloud needs a token. Add one in Settings.",
                )
            )
            drainAutoQueue()
            return
        }

        putAnswer(AiAnswer(sequence = sequence, question = event.content))

        autoJob = viewModelScope.launch {
            val answer = StringBuilder()
            try {
                repo.streamAnswer(
                    session.id,
                    event.content,
                    settings.assistantModel,
                    settings.assistantToken,
                    settings.assistantCloud,
                    answeredSequence = sequence,
                ).collect { chunk ->
                    if (chunk.delta.isNotEmpty()) {
                        answer.append(chunk.delta)
                        updateAnswer(sequence) { it.copy(text = answer.toString()) }
                    }
                }
                updateAnswer(sequence) { it.copy(streaming = false) }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                updateAnswer(sequence) {
                    it.copy(streaming = false, error = friendly(error))
                }
            } finally {
                autoJob = null
                // openSession/closeSession own clearing the queue; here we only
                // continue it when the same meeting is still on screen.
                val stillOpen = _state.value.openSession
                if (stillOpen?.id == session.id && stillOpen.aiHelp) {
                    drainAutoQueue()
                }
            }
        }
    }

    private fun putAnswer(answer: AiAnswer) {
        _state.update { it.copy(answers = it.answers + (answer.sequence to answer)) }
    }

    private fun updateAnswer(sequence: Long, change: (AiAnswer) -> AiAnswer) {
        _state.update { current ->
            val existing = current.answers[sequence] ?: return@update current
            current.copy(answers = current.answers + (sequence to change(existing)))
        }
    }

    private companion object {
        /**
         * Bounded backlog for interview help: enough for a few fast questions
         * while one answer streams, small enough that a slow model cannot fall
         * minutes behind the interviewer.
         */
        const val AUTO_QUEUE_LIMIT = 4

        /**
         * Cheap question heuristic for interview help. Committed lines are the
         * only input, so this is a filter, not a semantic classifier.
         */
        val QUESTION_STARTERS = listOf(
            "what", "why", "how", "when", "where", "who", "which",
            "can you", "could you", "would you", "will you",
            "do you", "did you", "are you", "have you", "is it", "are there",
            "tell me", "explain", "describe", "walk me through", "talk about",
            "give me an example", "suppose", "imagine",
        )
    }

    private fun friendly(error: Throwable): String {
        val status = when (error) {
            is StatusException -> error.status
            is StatusRuntimeException -> error.status
            else -> null
        }
        val description = status?.description.orEmpty()
        return when (status?.code) {
            Status.Code.UNAVAILABLE -> {
                // The server is up but its model endpoint is not; don't tell the
                // user their phone lost the network.
                if (description.startsWith("assistant provider:")) {
                    description.removePrefix("assistant provider:").trim()
                } else {
                    "Cannot reach the server. Check the address and network."
                }
            }
            Status.Code.UNAUTHENTICATED -> "The server rejected this device. Pair it again."
            Status.Code.PERMISSION_DENIED -> "This device is not allowed to do that."
            Status.Code.NOT_FOUND -> "That meeting no longer exists on the server."
            Status.Code.FAILED_PRECONDITION -> status.description ?: "The meeting has ended."
            Status.Code.DEADLINE_EXCEEDED -> "The server took too long to answer."
            Status.Code.INVALID_ARGUMENT -> status.description ?: "The server rejected the request."
            else -> error.message ?: error.javaClass.simpleName
        }
    }

    override fun onCleared() {
        streamJob?.cancel()
        statusJob?.cancel()
        assistantJob?.cancel()
        autoJob?.cancel()
        repository?.close()
        super.onCleared()
    }
}
