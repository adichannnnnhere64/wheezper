//! Assistant end-to-end tests against a mock Ollama HTTP endpoint.
//!
//! The mock also captures the prompt, which is how we prove the transcript
//! context actually reached the model and that answers are persisted without
//! touching transcript rows.

mod common;

use common::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wheezper_proto as proto;
use wheezper_proto::assistant_service_client::AssistantServiceClient;
use wheezper_proto::transcript_service_client::TranscriptServiceClient;

struct CapturedRequest {
    headers: String,
    body: String,
}

struct MockOllama {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl MockOllama {
    fn bodies(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("mock lock")
            .iter()
            .map(|request| request.body.clone())
            .collect()
    }

    fn headers(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("mock lock")
            .iter()
            .map(|request| request.headers.clone())
            .collect()
    }
}

/// Minimal HTTP server: answers `/api/chat`, streaming or not.
async fn mock_ollama(deltas: Vec<&'static str>, streaming: bool) -> MockOllama {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock bind");
    let url = format!("http://{}", listener.local_addr().expect("mock addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let captured = Arc::clone(&captured);
            let deltas = deltas.clone();
            tokio::spawn(async move {
                let Some((headers, body)) = read_request(&mut socket).await else {
                    return;
                };
                captured
                    .lock()
                    .expect("mock lock")
                    .push(CapturedRequest { headers, body });
                if streaming {
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(head.as_bytes()).await;
                    for delta in &deltas {
                        let line = serde_json::json!({
                            "message": {"role": "assistant", "content": delta},
                            "done": false
                        })
                        .to_string();
                        let _ = socket.write_all(line.as_bytes()).await;
                        let _ = socket.write_all(b"\n").await;
                    }
                    let _ = socket
                        .write_all(b"{\"message\":{\"content\":\"\"},\"done\":true}\n")
                        .await;
                } else {
                    let answer: String = deltas.concat();
                    let body = serde_json::json!({
                        "message": {"role": "assistant", "content": answer},
                        "done": true
                    })
                    .to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body.as_bytes()).await;
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    MockOllama { url, requests }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, String)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let mut body_start = None;
    let mut content_length = 0usize;
    loop {
        let n = socket.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if body_start.is_none()
            && let Some(pos) = find(&buf, b"\r\n\r\n")
        {
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            content_length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            body_start = Some(pos + 4);
        }
        if let Some(start) = body_start
            && buf.len() >= start + content_length
        {
            let headers = String::from_utf8_lossy(&buf[..start]).to_string();
            let body = String::from_utf8_lossy(&buf[start..]).to_string();
            return Some((headers, body));
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn seed_meeting(channel: &tonic::transport::Channel, device: &str) -> (String, String) {
    let (session_id, session_token) = create_session(channel, device, "Deployment review").await;
    let mut transcripts = TranscriptServiceClient::new(channel.clone());
    for (speaker, line) in [
        ("them", "We should deploy the new service next week."),
        ("me", "Security sign-off is still pending."),
    ] {
        transcripts
            .publish_transcript(request(
                event(&session_id, &new_uuid(), speaker, line),
                &session_token,
            ))
            .await
            .expect("seed transcript");
    }
    (session_id, session_token)
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_uses_transcript_context_and_persists_the_answer() {
    let Some(db) = database_url() else {
        eprintln!("{}", skip_reason());
        return;
    };
    let mock = mock_ollama(vec!["Deploy on Tuesday, after security sign-off."], false).await;
    let Some(server) = TestServer::start_with(db, mock.url.clone()).await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = seed_meeting(&channel, &device).await;

    let answer = AssistantServiceClient::new(channel.clone())
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "When can we deploy?".into(),
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("ask question")
        .into_inner();
    assert_eq!(answer.answer, "Deploy on Tuesday, after security sign-off.");
    assert_eq!(answer.provider, "ollama");
    assert_eq!(answer.model, "test-model");

    // The model really saw the meeting, not just the question.
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("We should deploy the new service next week."));
    assert!(bodies[0].contains("Security sign-off is still pending."));
    assert!(bodies[0].contains("When can we deploy?"));
    assert!(bodies[0].contains("\"think\":false"));

    // Persisted next to the session...
    let pool = server.pool().await;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM assistant_messages WHERE session_id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).expect("uuid"))
            .fetch_one(&pool)
            .await
            .expect("count assistant messages");
    assert_eq!(count, 1);

    // ...but never mixed into the transcript.
    let sync = TranscriptServiceClient::new(channel)
        .sync_transcripts(request(
            proto::SyncTranscriptsRequest {
                session_id: session_id.clone(),
                from_sequence: 0,
                limit: 50,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync.events.len(), 2);
    assert!(
        sync.events
            .iter()
            .all(|e| !e.content.contains("Deploy on Tuesday"))
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_answer_yields_deltas_then_done() {
    let Some(db) = database_url() else {
        eprintln!("{}", skip_reason());
        return;
    };
    let mock = mock_ollama(vec!["Ask about ", "Tokio ", "and shared state."], true).await;
    let Some(server) = TestServer::start_with(db, mock.url.clone()).await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = seed_meeting(&channel, &device).await;

    let mut stream = AssistantServiceClient::new(channel)
        .stream_answer(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "What should I answer about concurrency?".into(),
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("stream answer")
        .into_inner();

    let mut text = String::new();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.message())
            .await
            .expect("chunk timeout")
            .expect("stream error")
            .expect("stream ended early");
        text.push_str(&chunk.delta);
        if chunk.done {
            break;
        }
    }
    assert_eq!(text, "Ask about Tokio and shared state.");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_can_choose_its_model_and_supply_its_own_token() {
    let Some(db) = database_url() else {
        eprintln!("{}", skip_reason());
        return;
    };
    let mock = mock_ollama(vec!["Use Tokio tasks and a mutex."], false).await;
    let Some(server) = TestServer::start_with(db, mock.url.clone()).await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = seed_meeting(&channel, &device).await;

    let answer = AssistantServiceClient::new(channel)
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id,
                question: "How would you handle concurrency?".into(),
                model: "deepseek-v4.1-flash".into(),
                api_key: "user-secret-token".into(),
                backend: proto::AssistantBackend::Default as i32,
                answered_sequence: 0,
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("ask with overrides")
        .into_inner();

    assert_eq!(answer.model, "deepseek-v4.1-flash");
    assert_eq!(answer.provider, "ollama");

    // The provider saw the chosen model and the caller's token, not the
    // server's configuration.
    let bodies = mock.bodies();
    assert!(bodies[0].contains("\"model\":\"deepseek-v4.1-flash\""));
    let headers = mock.headers();
    assert!(
        headers[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer user-secret-token"),
        "missing per-request bearer token: {}",
        headers[0]
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_provider_is_reported_as_unavailable() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = seed_meeting(&channel, &device).await;

    let err = AssistantServiceClient::new(channel)
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id,
                question: "anything".into(),
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect_err("no provider is running on the test port");
    assert_eq!(err.code(), tonic::Code::Unavailable);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_are_anchored_to_their_row_and_become_model_context() {
    let Some(db) = database_url() else {
        eprintln!("{}", skip_reason());
        return;
    };
    let mock = mock_ollama(vec!["Guard it with a mutex."], false).await;
    let Some(server) = TestServer::start_with(db, mock.url.clone()).await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = seed_meeting(&channel, &device).await;

    let mut assistant = AssistantServiceClient::new(channel.clone());
    assistant
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "How would you handle concurrency?".into(),
                // The far side's line is sequence 1 in seed_meeting.
                answered_sequence: 1,
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("ask about a transcript row");

    // The answer comes back anchored, so the phone can render it under the row
    // again after a restart instead of losing it with the app process.
    let answers = assistant
        .list_answers(request(
            proto::ListAnswersRequest {
                session_id: session_id.clone(),
            },
            &device,
        ))
        .await
        .expect("list answers")
        .into_inner()
        .answers;
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].answered_sequence, 1);
    assert_eq!(answers[0].question, "How would you handle concurrency?");
    assert_eq!(answers[0].answer, "Guard it with a mutex.");

    // A follow-up question carries the earlier answer as context: the assistant
    // must not contradict itself just because it cannot see its own reply.
    assistant
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "And for reads?".into(),
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("follow-up question");
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies[1].contains("What you already answered in this meeting"),
        "the previous answer is missing from the prompt: {}",
        bodies[1]
    );
    assert!(
        bodies[1].contains("Guard it with a mutex."),
        "the previous answer text is missing: {}",
        bodies[1]
    );

    // A session that does not exist is an error, not a silent empty list.
    assert!(
        assistant
            .list_answers(request(
                proto::ListAnswersRequest {
                    session_id: uuid::Uuid::new_v4().to_string(),
                },
                &device,
            ))
            .await
            .is_err()
    );
    server.stop().await;
}

/// The per-session brief ("what this interview is about") and the answer
/// guidance the user typed must reach the model — and must be editable
/// mid-meeting, because the useful brief is written after the first question.
#[tokio::test(flavor = "multi_thread")]
async fn session_brief_and_guidance_reach_the_prompt() {
    use wheezper_proto::session_service_client::SessionServiceClient;

    let Some(db) = database_url() else {
        eprintln!("{}", skip_reason());
        return;
    };
    let mock = mock_ollama(vec!["I have five years with Spring Boot."], false).await;
    let Some(server) = TestServer::start_with(db, mock.url.clone()).await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = create_session_with_ai(&channel, &device, "Interview", true).await;

    let updated = SessionServiceClient::new(channel.clone())
        .update_session_guidance(request(
            proto::UpdateSessionGuidanceRequest {
                session_id: session_id.clone(),
                ai_context: "Senior Java backend interview; the team runs Spring Boot on \
                             Kubernetes and cares about transactions."
                    .into(),
                ai_instructions: "Answer in the first person, one sentence, and correct my \
                                  grammar in your answer."
                    .into(),
            },
            &device,
        ))
        .await
        .expect("update guidance")
        .into_inner();
    assert!(updated.ai_context.contains("Spring Boot"));
    assert!(updated.ai_instructions.contains("correct my grammar"));

    AssistantServiceClient::new(channel.clone())
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "How much Spring experience do you have?".into(),
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("ask question");

    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].contains("Senior Java backend interview"),
        "the session brief is missing from the prompt: {}",
        bodies[0]
    );
    assert!(
        bodies[0].contains("correct my grammar"),
        "the answer guidance is missing from the prompt: {}",
        bodies[0]
    );
    assert!(
        bodies[0].contains("Follow it where it does not conflict with the rules above"),
        "guidance must not override the prompt's own rules: {}",
        bodies[0]
    );

    // The brief is a prompt field, never a transcript row.
    let sync = TranscriptServiceClient::new(channel)
        .sync_transcripts(request(
            proto::SyncTranscriptsRequest {
                session_id,
                from_sequence: 0,
                limit: 50,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(sync.events.is_empty());
    server.stop().await;
}

/// Double-tapping a row asks for detail: the interview prompt switches to a
/// few sentences with trade-offs, while the default stays the one-liner and
/// the "never invent personal facts" rule is unchanged in both.
#[tokio::test(flavor = "multi_thread")]
async fn explain_more_asks_for_a_deeper_answer() {
    let Some(db) = database_url() else {
        eprintln!("{}", skip_reason());
        return;
    };
    let mock = mock_ollama(vec!["A record is a shallow immutable carrier.", "Deeper answer."], false).await;
    let Some(server) = TestServer::start_with(db, mock.url.clone()).await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, _) = create_session_with_ai(&channel, &device, "Interview", true).await;
    let mut assistant = AssistantServiceClient::new(channel);

    assistant
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "What is a record in Java?".into(),
                explain_more: true,
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("explain more");
    assistant
        .ask_question(request(
            proto::AskQuestionRequest {
                session_id: session_id.clone(),
                question: "What is a record in Java?".into(),
                ..Default::default()
            },
            &device,
        ))
        .await
        .expect("plain question");

    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies[0].contains("2-4 sentences of concrete technical detail"),
        "the detail prompt did not reach the model: {}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("one conversational sentence of 10-20 words"),
        "the default prompt changed: {}",
        bodies[1]
    );
    for body in &bodies {
        assert!(
            body.contains("Never invent personal facts"),
            "the fact rule must survive both shapes: {body}"
        );
    }
    server.stop().await;
}
