package com.wheezper.app

import com.wheezper.app.data.MeetingRepository
import com.wheezper.app.data.WheezperClient
import com.wheezper.v1.TranscriptEvent
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test

/**
 * Optional live test: the Android gRPC client against a running
 * wheezper-server. Skipped unless both env vars are set, so `./gradlew test`
 * works on a machine with no backend.
 *
 *   WHEEZPER_TEST_SERVER=http://127.0.0.1:50051 \
 *   WHEEZPER_TEST_PAIRING_CODE=... ./gradlew testDebugUnitTest
 */
class WheezperClientTest {
    @Test
    fun pairCreatePublishAndReadBack() = runBlocking {
        val server = env("WHEEZPER_TEST_SERVER")
        val code = env("WHEEZPER_TEST_PAIRING_CODE")
        assumeTrue(
            "set WHEEZPER_TEST_SERVER and WHEEZPER_TEST_PAIRING_CODE to run",
            server != null && code != null,
        )

        val paired = MeetingRepository.pairDevice(server!!, code!!, "android-jvm-test")
        assertTrue(paired.deviceToken.isNotBlank())

        WheezperClient(server, paired.deviceToken).use { device ->
            val repo = MeetingRepository(device)
            val created = repo.createSession("android jvm test", aiHelp = true)
            assertTrue(created.session.id.isNotBlank())
            assertTrue(created.sessionToken.isNotBlank())
            assertTrue("ai_help must round-trip", created.session.aiHelp)
            assertTrue(
                "getSession must report ai_help",
                repo.getSession(created.session.id).aiHelp,
            )

            // The session-scoped token is enough for publishing: no device
            // credential is needed on the laptop side.
            WheezperClient(server, created.sessionToken).use { sessionClient ->
                val published = sessionClient.transcripts.publishTranscript(
                    TranscriptEvent.newBuilder()
                        .setEventId(java.util.UUID.randomUUID().toString())
                        .setSessionId(created.session.id)
                        .setSpeaker("them")
                        .setContent("The Android client published this line.")
                        .build()
                )
                assertTrue(published.sequenceNumber > 0)
            }

            val sync = repo.sync(created.session.id, 0)
            assertEquals(1, sync.eventsCount)
            assertEquals("The Android client published this line.", sync.eventsList[0].content)

            val sessions = repo.listSessions(activeOnly = false)
            assertTrue(sessions.any { it.id == created.session.id })
        }
    }

    private fun env(name: String): String? = System.getenv(name)?.takeIf { it.isNotBlank() }
}
