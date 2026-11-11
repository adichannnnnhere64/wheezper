package com.wheezper.app.data

import io.grpc.ManagedChannel
import io.grpc.ManagedChannelBuilder
import io.grpc.Metadata
import io.grpc.stub.AbstractStub
import io.grpc.stub.MetadataUtils
import java.net.URI
import java.util.concurrent.TimeUnit
import com.wheezper.v1.AnswerChunk
import com.wheezper.v1.AssistantAnswer
import com.wheezper.v1.AssistantBackend
import com.wheezper.v1.AssistantServiceGrpcKt
import com.wheezper.v1.AuthServiceGrpcKt
import com.wheezper.v1.CreateSessionResponse
import com.wheezper.v1.ListSessionsRequest
import com.wheezper.v1.PairDeviceRequest
import com.wheezper.v1.PairDeviceResponse
import com.wheezper.v1.Session
import com.wheezper.v1.SessionServiceGrpcKt
import com.wheezper.v1.SessionStatus
import com.wheezper.v1.StreamTranscriptsRequest
import com.wheezper.v1.SyncTranscriptsRequest
import com.wheezper.v1.SyncTranscriptsResponse
import com.wheezper.v1.TranscriptEvent
import com.wheezper.v1.TranscriptServiceGrpcKt
import com.wheezper.v1.AskQuestionRequest
import com.wheezper.v1.ListAnswersRequest
import kotlinx.coroutines.flow.Flow

/**
 * Thin gRPC client for one paired device. Every call carries the device token
 * as a bearer credential; no key or secret is compiled into the app.
 */
class WheezperClient(serverUrl: String, private val deviceToken: String?) : AutoCloseable {
    private val channel: ManagedChannel = buildChannel(serverUrl)

    private fun <T : AbstractStub<T>> authenticated(stub: T): T =
        if (deviceToken.isNullOrBlank()) stub
        else stub.withInterceptors(
            MetadataUtils.newAttachHeadersInterceptor(
                Metadata().apply { put(AUTH_KEY, "Bearer $deviceToken") }
            )
        )

    val sessions: SessionServiceGrpcKt.SessionServiceCoroutineStub
        get() = authenticated(SessionServiceGrpcKt.SessionServiceCoroutineStub(channel))

    val transcripts: TranscriptServiceGrpcKt.TranscriptServiceCoroutineStub
        get() = authenticated(TranscriptServiceGrpcKt.TranscriptServiceCoroutineStub(channel))

    val assistant: AssistantServiceGrpcKt.AssistantServiceCoroutineStub
        get() = authenticated(AssistantServiceGrpcKt.AssistantServiceCoroutineStub(channel))

    val auth: AuthServiceGrpcKt.AuthServiceCoroutineStub
        get() = AuthServiceGrpcKt.AuthServiceCoroutineStub(channel)

    override fun close() {
        channel.shutdownNow()
        runCatching { channel.awaitTermination(1, TimeUnit.SECONDS) }
    }

    companion object {
        private val AUTH_KEY: Metadata.Key<String> =
            Metadata.Key.of("authorization", Metadata.ASCII_STRING_MARSHALLER)

        private fun buildChannel(raw: String): ManagedChannel {
            val url = when {
                raw.startsWith("http://") || raw.startsWith("https://") -> raw
                else -> "http://${raw.trim()}"
            }
            val uri = URI(url)
            val secure = uri.scheme.equals("https", ignoreCase = true)
            val host = uri.host ?: error("server address needs a host: $raw")
            val port = when {
                uri.port > 0 -> uri.port
                secure -> 443
                else -> 50051
            }
            val builder = ManagedChannelBuilder.forAddress(host, port)
                .keepAliveTime(30, TimeUnit.SECONDS)
                .keepAliveWithoutCalls(true)
            if (secure) builder.useTransportSecurity() else builder.usePlaintext()
            return builder.build()
        }
    }
}

/** Session, transcript and assistant use-cases over the gRPC contract. */
class MeetingRepository(private val client: WheezperClient) : AutoCloseable {

    suspend fun listSessions(activeOnly: Boolean, limit: Int = 50): List<Session> =
        client.sessions.listSessions(
            ListSessionsRequest.newBuilder().setLimit(limit).setActiveOnly(activeOnly).build()
        ).sessionsList

    suspend fun getSession(sessionId: String): Session =
        client.sessions.getSession(
            com.wheezper.v1.GetSessionRequest.newBuilder().setSessionId(sessionId).build()
        )

    suspend fun createSession(title: String, aiHelp: Boolean): CreateSessionResponse =
        client.sessions.createSession(
            com.wheezper.v1.CreateSessionRequest.newBuilder()
                .setTitle(title)
                .setAiHelp(aiHelp)
                .build()
        )

    suspend fun endSession(sessionId: String): Session =
        client.sessions.endSession(
            com.wheezper.v1.EndSessionRequest.newBuilder().setSessionId(sessionId).build()
        ).session

    suspend fun sync(sessionId: String, fromSequence: Long): SyncTranscriptsResponse =
        client.transcripts.syncTranscripts(
            SyncTranscriptsRequest.newBuilder()
                .setSessionId(sessionId)
                .setFromSequence(fromSequence)
                .setLimit(500)
                .build()
        )

    fun streamTranscripts(sessionId: String, fromSequence: Long): Flow<TranscriptEvent> =
        client.transcripts.streamTranscripts(
            StreamTranscriptsRequest.newBuilder()
                .setSessionId(sessionId)
                .setFromSequence(fromSequence)
                .build()
        )

    /**
     * The phone chooses the model and (for Ollama Cloud) its own token. The
     * token is used by the server for this call only; the endpoint is fixed by
     * the backend, never supplied here.
     */
    fun streamAnswer(
        sessionId: String,
        question: String,
        model: String,
        apiKey: String,
        cloud: Boolean,
        answeredSequence: Long = 0,
    ): Flow<AnswerChunk> =
        client.assistant.streamAnswer(
            AskQuestionRequest.newBuilder()
                .setSessionId(sessionId)
                .setQuestion(question)
                .setModel(model.trim())
                .setApiKey(apiKey.trim())
                // 0 = the free-form sheet, which answers no single transcript row.
                .setAnsweredSequence(answeredSequence)
                .setBackend(
                    if (cloud) {
                        AssistantBackend.ASSISTANT_BACKEND_OLLAMA_CLOUD
                    } else {
                        AssistantBackend.ASSISTANT_BACKEND_DEFAULT
                    }
                )
                .build()
        )

    /**
     * Answers the server already has for this meeting, one per transcript row.
     * Rendering them under their rows is what makes an answer survive a restart
     * (they used to live only in the app's memory).
     */
    suspend fun listAnswers(sessionId: String): List<AssistantAnswer> =
        client.assistant.listAnswers(
            ListAnswersRequest.newBuilder().setSessionId(sessionId).build()
        ).answersList

    override fun close() = client.close()

    companion object {
        /** Exchange the operator's pairing code for a short-lived device token. */
        suspend fun pairDevice(
            serverUrl: String,
            pairingCode: String,
            deviceName: String,
        ): PairDeviceResponse = WheezperClient(serverUrl, null).use { client ->
            client.auth.pairDevice(
                PairDeviceRequest.newBuilder()
                    .setPairingCode(pairingCode)
                    .setDeviceName(deviceName)
                    .build()
            )
        }

        fun isCompleted(session: Session): Boolean =
            session.status == SessionStatus.SESSION_STATUS_COMPLETED
    }
}
