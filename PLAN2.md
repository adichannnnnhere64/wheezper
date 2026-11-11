# Wheezper Connect — Project Architecture & Implementation Plan

**Project type:** Real-time meeting transcription and mobile companion system  
**Primary languages:** Rust, Kotlin  
**Communication:** gRPC / Protocol Buffers  
**Core components:** Wheezper CLI, Rust Backend, Android Application

---

# 1. Project Vision

Transform Wheezper from a standalone terminal transcription agent into an optional real-time meeting transcription platform.

The user runs Wheezper on a laptop, where audio capture and transcription happen locally.

A separate Rust backend receives transcription events through gRPC, stores them permanently, and distributes them to connected clients.

An Android application connects to the backend and provides a live view of the meeting.

The Android application should also allow the user to ask questions about the ongoing meeting and receive AI-generated suggested answers using the meeting transcript as context.

## Main objectives

1. Preserve the original Wheezper functionality.
2. Add optional session-based synchronization.
3. Stream committed transcriptions to a Rust server in real time.
4. Persist meeting sessions and their transcripts.
5. Display live transcripts on Android.
6. Allow the user to ask AI questions about a meeting.
7. Support reconnecting without losing transcription events.
8. Keep the architecture lightweight and efficient.

# 2. High-Level Architecture

```text
                  LAPTOP
        ┌───────────────────────────┐
        │         Wheezper          │
        │                           │
        │  Microphone               │
        │  System Audio             │
        │       │                   │
        │       ▼                   │
        │  Whisper.cpp              │
        │       │                   │
        │       ▼                   │
        │  Transcript Pipeline      │
        │       │                   │
        │       ├── Local Markdown  │
        │       │                   │
        │       └── gRPC Client     │
        └────────────┬──────────────┘
                     │
                     │ gRPC
                     │
                     ▼
        ┌───────────────────────────┐
        │       RUST BACKEND        │
        │                           │
        │  Session Manager          │
        │  Transcript Ingestion     │
        │  Event Distributor        │
        │  AI Answer Engine         │
        │  Authentication           │
        │                           │
        │       ┌───────────┐       │
        │       │ Database  │       │
        │       │           │       │
        │       │ Sessions  │       │
        │       │ Transcript│       │
        │       │ Answers   │       │
        │       └───────────┘       │
        └────────────┬──────────────┘
                     │
                     │ gRPC
                     │
                     ▼
        ┌───────────────────────────┐
        │      ANDROID APP          │
        │                           │
        │  Active Meetings          │
        │  Live Transcription       │
        │  Meeting History          │
        │  AI Assistant             │
        │  Search Transcripts       │
        └───────────────────────────┘
```

# 3. Component A — Wheezper Integration

Repository: https://github.com/adichannnnnhere64/wheezper

Wheezper remains the transcription engine.

Do not rewrite its existing audio capture, Whisper integration, segmentation, or local Markdown functionality.

Instead, introduce an optional remote session transport.

## New CLI arguments

Example:

```bash
# Existing behavior remains unchanged
wheezper --source dual

# Connect to a remote meeting session
wheezper \
  --source dual \
  --session SESSION_ID \
  --server https://meeting.example.com:50051 \
  --session-token TOKEN

# Existing local agent can still work
wheezper \
  --source dual \
  --with-agent ollama:gemma3:1b:256 \
  --session SESSION_ID \
  --server https://meeting.example.com:50051
```

The exact authentication mechanism should eventually support secure device pairing instead of requiring users to manually manage permanent tokens.

## Required behavior

When `--session` is omitted:

- Wheezper behaves exactly as it currently does.
- No backend connection is attempted.
- No network dependency is introduced.
- Existing CLI commands and configurations continue working.

When `--session` is supplied:

1. Connect to the configured backend.
2. Authenticate the session.
3. Begin sending committed transcript events.
4. Preserve local Markdown output.
5. Retry connections when the backend becomes unavailable.
6. Buffer unsent events locally.
7. Resume transmission after reconnecting.

## Important transcription rule

Wheezper currently has two transcription stages:

- Partial preview: provisional text that can change.
- Committed transcript: finalized text that is written to the notes file.

Initially, synchronize only committed transcript events.

This avoids duplicated or incorrect transcript entries.

Partial transcription synchronization can be introduced later as an optional feature.

## Proposed internal design

Introduce a separate sink/transport abstraction:

```rust
trait TranscriptSink {
    async fn publish(
        &self,
        event: TranscriptEvent,
    ) -> Result<(), SinkError>;
}
```

Possible implementations:

- MarkdownSink
- WebhookSink
- GrpcSink

Do not let gRPC code directly modify the Whisper transcription pipeline.

A failure in GrpcSink must never prevent local transcription.

# 4. Component B — Rust Backend

This is the central component of the new platform.

## Recommended stack

| Component | Technology |
|---|---|
| Language | Rust |
| Async runtime | Tokio |
| gRPC | Tonic |
| Protocol | Protocol Buffers |
| Database | PostgreSQL |
| Database access | SQLx |
| Migrations | SQLx migrations |
| Serialization | Prost |
| Logging | tracing |
| Configuration | Figment or environment variables |
| Authentication | Short-lived session credentials |

Keep the architecture modular, but start with a modular monolith instead of microservices.

## Suggested project structure

```text
wheezper-server/
├── Cargo.toml
├── proto/
│   └── wheezper/
│       └── v1/
│           ├── session.proto
│           ├── transcript.proto
│           └── assistant.proto
│
├── migrations/
│
├── src/
│   ├── main.rs
│   ├── config.rs
│   │
│   ├── grpc/
│   │   ├── mod.rs
│   │   ├── session.rs
│   │   ├── transcript.rs
│   │   └── assistant.rs
│   │
│   ├── session/
│   │   ├── mod.rs
│   │   ├── service.rs
│   │   └── repository.rs
│   │
│   ├── transcript/
│   │   ├── mod.rs
│   │   ├── service.rs
│   │   ├── repository.rs
│   │   └── broadcaster.rs
│   │
│   ├── assistant/
│   │   ├── mod.rs
│   │   ├── service.rs
│   │   ├── context.rs
│   │   └── provider.rs
│   │
│   ├── auth/
│   ├── database/
│   └── error.rs
│
└── tests/
```

## Session management

The backend must support:

- Creating sessions.
- Starting sessions.
- Connecting transcription clients.
- Connecting mobile clients.
- Listing sessions.
- Retrieving historical transcripts.
- Ending sessions.
- Reconnecting to active sessions.

Suggested session states:

```text
CREATED
   |
   ▼
ACTIVE
   |
   ▼
COMPLETED
```

A disconnected laptop must not automatically destroy or complete its session.

The server should distinguish between a session's lifecycle and its currently connected devices.

## Database design

### sessions

| Column | Type |
|---|---|
| id | UUID |
| title | TEXT |
| status | VARCHAR |
| started_at | TIMESTAMPTZ |
| ended_at | TIMESTAMPTZ |
| created_at | TIMESTAMPTZ |

### transcript_entries

| Column | Type |
|---|---|
| id | UUID |
| session_id | UUID |
| event_id | UUID |
| speaker | VARCHAR |
| content | TEXT |
| started_at | TIMESTAMPTZ |
| received_at | TIMESTAMPTZ |
| sequence_number | BIGINT |

Important constraints:

- `event_id` must be unique.
- `sequence_number` must be unique within a session.
- Transcript entries must be append-only initially.
- Repeated events must not create duplicate entries.

### assistant_messages

| Column | Type |
|---|---|
| id | UUID |
| session_id | UUID |
| question | TEXT |
| answer | TEXT |
| created_at | TIMESTAMPTZ |

# 5. gRPC Communication Design

Use Protocol Buffers as the shared contract between Wheezper, the backend, and Android.

Create versioned protobuf definitions.

## SessionService

Responsibilities:

```protobuf
service SessionService {
  rpc CreateSession(CreateSessionRequest)
      returns (CreateSessionResponse);

  rpc GetSession(GetSessionRequest)
      returns (Session);

  rpc ListSessions(ListSessionsRequest)
      returns (ListSessionsResponse);

  rpc EndSession(EndSessionRequest)
      returns (EndSessionResponse);
}
```

## TranscriptService

Responsibilities:

```protobuf
service TranscriptService {
  rpc PublishTranscript(TranscriptEvent)
      returns (PublishTranscriptResponse);

  rpc StreamTranscripts(StreamTranscriptsRequest)
      returns (stream TranscriptEvent);

  rpc SyncTranscripts(SyncTranscriptsRequest)
      returns (SyncTranscriptsResponse);
}
```

The laptop publishes events.

Android subscribes to transcript events.

The server persists each committed event before broadcasting it to connected clients.

## AssistantService

Responsibilities:

```protobuf
service AssistantService {
  rpc AskQuestion(AskQuestionRequest)
      returns (AskQuestionResponse);

  rpc StreamAnswer(AskQuestionRequest)
      returns (stream AnswerChunk);
}
```

The assistant receives the session ID and question, retrieves relevant meeting context, and generates an answer.

# 6. Event Structure

Every transcript event should contain enough information to support persistence, ordering, and reconnection.

Example:

```json
{
  "event_id": "uuid",
  "session_id": "uuid",
  "sequence_number": 15,
  "speaker": "them",
  "content": "We should deploy the new service next week.",
  "started_at": "2026-11-11T02:00:00Z",
  "received_at": "2026-11-11T02:00:04Z",
  "source": "wheezper"
}
```

Use event IDs for idempotency.

Use sequence numbers for reliable ordering.

The server should acknowledge accepted events.

If Wheezper disconnects, it should resend events that have not been acknowledged.

The server must safely ignore already persisted event IDs.

# 7. Reliable Delivery and Reconnection

This is a critical requirement.

Do not assume gRPC guarantees that events are permanently delivered.

Implement an application-level delivery mechanism.

### Laptop behavior

1. Transcribe locally.
2. Append the transcript to Markdown.
3. Persist the outgoing event to a local pending-event queue.
4. Attempt to transmit it.
5. Wait for server acknowledgement.
6. Mark it acknowledged locally.
7. Retry unacknowledged events after reconnection.

### Backend behavior

1. Authenticate the client.
2. Validate the session.
3. Check event ID.
4. Persist the event.
5. Assign or validate its sequence.
6. Acknowledge the event.
7. Broadcast the persisted event.

### Android behavior

1. Subscribe to the active session.
2. Receive ordered transcript events.
3. Display them immediately.
4. Detect sequence gaps.
5. Request synchronization if events are missing.
6. Reconnect automatically after network interruption.

Use PostgreSQL as the durable source of truth.

An in-memory broadcast channel should only distribute events, never serve as permanent storage.

# 8. Component C — Android Application

The Android app is a lightweight companion, not another transcription engine.

Its primary job is to connect to the Rust backend and display meeting information.

## Recommended technology

- Kotlin
- Jetpack Compose
- Coroutines and Flow
- gRPC Kotlin / grpc-java
- Room for optional local caching
- DataStore for application settings
- WorkManager for appropriate background synchronization

Keep the application lightweight.

Do not implement Whisper transcription inside Android.

## Main screens

### 1. Dashboard

Display:

- Active meetings.
- Recent meetings.
- Connection status.
- Create session action.

### 2. Meeting Session

The primary screen.

Display:

- Meeting title.
- Session status.
- Live transcript.
- Speaker labels.
- Timestamps.
- Connection status.
- AI assistant access.

Transcript updates should appear automatically without refreshing the screen.

### 3. AI Assistant

Allow the user to ask questions while a meeting is happening.

Example questions:

- What did they say about the deployment deadline?
- What questions did the interviewer ask?
- What should I answer?
- Summarize the last five minutes.
- What action items were mentioned?
- What was the agreed budget?

Display the response separately from the original transcript.

The AI must not silently modify or overwrite transcript entries.

### 4. Meeting History

Display:

- Previous meetings.
- Meeting date.
- Duration.
- Transcript preview.
- Search functionality.
- Ability to reopen a completed meeting.

### 5. Settings

Include:

- Backend server address.
- Device pairing.
- Connection status.
- Notification preferences.
- Local cache settings.

## UI direction

Use a minimalistic, low-poly-inspired visual language.

Avoid heavy gradients, excessive animations, and unnecessary interface elements.

Prioritize:

- Readability.
- Fast rendering.
- Dark mode.
- Clear speaker distinction.
- Minimal navigation.
- Low battery consumption.

# 9. AI Assistant Architecture

The assistant is a separate backend capability.

Do not make Android responsible for calling an LLM directly.

The backend should provide an abstraction for different AI providers.

```rust
trait AssistantProvider {
    async fn generate(
        &self,
        context: MeetingContext,
        question: String,
    ) -> Result<String, AssistantError>;
}
```

Potential providers:

- Ollama.
- OpenAI-compatible APIs.
- Other hosted LLM providers.
- Future local inference providers.

## Context retrieval

When a question is received:

1. Retrieve the session.
2. Load the recent transcript.
3. Search older transcript entries when needed.
4. Build a bounded context.
5. Send the context and question to the selected provider.
6. Return the answer to Android.
7. Optionally persist the question and answer.

Later, introduce embeddings and vector search for large meeting histories.

For the MVP, PostgreSQL full-text search and recent transcript retrieval are sufficient.

## Example

Transcript:

```text
them: Can you explain your experience with Rust?
me: I have been developing backend applications...
them: How would you handle concurrent requests?
```

Question:

```text
What should I answer about concurrency?
```

Assistant response:

```text
Explain your experience with Tokio,
async tasks, shared state, and synchronization.
Give a practical example from your backend work.
```

The assistant should clearly distinguish generated suggestions from actual meeting statements.

# 10. Security and Privacy

Meeting transcripts can contain confidential information.

Implement security from the beginning.

Requirements:

- TLS for remote gRPC connections.
- Authenticated device connections.
- Short-lived pairing credentials.
- Session-scoped authorization.
- No public access to transcripts.
- No credentials hardcoded in the Android app.
- Configurable transcript retention.
- Explicit user control over session deletion.
- No automatic audio uploads.

Only transcript text should be transmitted by default.

Raw audio remains on the laptop.

AI providers should be configurable, with clear disclosure when transcript content is sent to an external provider.

# 11. Development Milestones

Implement this incrementally.

## Phase 1 — Backend Foundation

Goal: A working Rust server with session persistence.

Tasks:

- Initialize Rust workspace.
- Configure Tokio and Tonic.
- Create protobuf definitions.
- Implement SessionService.
- Implement PostgreSQL schema and migrations.
- Add session creation and retrieval.
- Add basic authentication.
- Create integration tests.

Acceptance criteria:

- Can create a session.
- Can retrieve a session.
- Can end a session.
- Session data survives server restarts.

## Phase 2 — Wheezper gRPC Integration

Goal: Send actual transcriptions to the backend.

Tasks:

- Introduce optional `--session`.
- Introduce `--server`.
- Introduce session authentication.
- Implement GrpcSink.
- Send committed transcript events.
- Preserve existing Markdown output.
- Add local pending-event queue.
- Implement acknowledgement and retry logic.

Acceptance criteria:

- Existing Wheezper behavior remains unchanged.
- A connected Wheezper instance sends transcripts to the server.
- Disconnecting the backend does not stop local transcription.
- Reconnecting delivers pending events without duplication.

## Phase 3 — Android Foundation

Goal: Display live transcripts.

Tasks:

- Initialize Android application.
- Configure gRPC client.
- Implement device pairing.
- Implement session listing.
- Implement session subscription.
- Build live transcript UI.
- Add connection state handling.
- Add sequence-gap recovery.

Acceptance criteria:

- Android can connect to the backend.
- Android can display active sessions.
- New transcript entries appear in real time.
- Reconnecting restores the correct transcript state.

## Phase 4 — AI Assistant

Goal: Ask questions about active meetings.

Tasks:

- Implement AssistantService.
- Create provider abstraction.
- Implement Ollama integration.
- Build transcript context retrieval.
- Implement AskQuestion.
- Implement streaming answers.
- Add Android assistant interface.
- Persist assistant conversations.

Acceptance criteria:

- Android can ask questions about an active meeting.
- The backend uses the correct meeting context.
- Answers are returned without modifying transcripts.

## Phase 5 — Meeting History and Polish

Tasks:

- Historical session listing.
- Transcript pagination.
- Search functionality.
- Meeting summaries.
- Session export.
- Notification support.
- Reconnection testing.
- Performance optimization.
- Release packaging.

# 12. Important Engineering Rules

These rules must be followed throughout development.

1. Never break Wheezper's existing standalone behavior.
2. Do not move Whisper transcription to the backend.
3. Do not transmit raw audio in the initial implementation.
4. Do not make local Markdown output dependent on network connectivity.
5. Do not block the transcription pipeline while sending gRPC events.
6. Use asynchronous queues and bounded buffers.
7. Persist events before acknowledging them.
8. Implement idempotency for transcript ingestion.
9. Use sequence numbers for event ordering and synchronization.
10. Version all protobuf contracts.
11. Keep backend services modular.
12. Do not introduce microservices prematurely.
13. Avoid unnecessary dependencies in the Wheezper binary.
14. Add integration tests for disconnects, retries, and duplicate events.

# 13. Definition of Done

The project is successful when the following workflow works end to end:

1. Open the Android application.
2. Create a meeting session.
3. Start Wheezper on the laptop with the session ID.
4. Wheezper begins transcribing microphone and system audio locally.
5. Committed transcript entries are sent to the Rust backend.
6. The backend stores each transcript entry.
7. Android receives the entries in real time.
8. The user can scroll through the live transcript.
9. The user can ask the AI assistant questions about the meeting.
10. The assistant returns contextual suggestions.
11. The laptop loses its internet connection.
12. Wheezper continues transcribing locally.
13. The connection returns.
14. Pending events synchronize automatically.
15. The meeting transcript remains complete and available in history.

**Final architectural principle:**

Wheezper is the local transcription engine. The Rust backend is the durable session and intelligence layer. Android is the mobile viewing and interaction layer.

These are separate components with clearly defined responsibilities, connected through versioned gRPC contracts.

