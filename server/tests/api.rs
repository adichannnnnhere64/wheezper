//! End-to-end gRPC tests against a real server and a real PostgreSQL.
//!
//! These cover the reliability rules from PLAN2: idempotent ingestion, ordered
//! sequence numbers, session-scoped authorization, reconnect catch-up and
//! durability across a server restart.

mod common;

use common::*;
use std::time::Duration;
use tonic::Code;
use wheezper_proto as proto;
use wheezper_proto::session_service_client::SessionServiceClient;
use wheezper_proto::transcript_service_client::TranscriptServiceClient;

#[tokio::test(flavor = "multi_thread")]
async fn pairing_requires_the_operator_code() {
    use wheezper_proto::auth_service_client::AuthServiceClient;
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let mut auth = AuthServiceClient::new(channel);

    let err = auth
        .pair_device(proto::PairDeviceRequest {
            pairing_code: "wrong-code".into(),
            device_name: "test".into(),
        })
        .await
        .expect_err("wrong pairing code must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated);

    let response = auth
        .pair_device(proto::PairDeviceRequest {
            pairing_code: PAIRING_CODE.into(),
            device_name: "test".into(),
        })
        .await
        .expect("pair")
        .into_inner();
    assert!(response.device_token.len() >= 32);
    assert!(response.expires_at.is_some());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_rpcs_require_a_token() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let mut sessions = SessionServiceClient::new(channel);

    let err = sessions
        .list_sessions(proto::ListSessionsRequest {
            limit: 10,
            active_only: false,
        })
        .await
        .expect_err("unauthenticated list must fail");
    assert_eq!(err.code(), Code::Unauthenticated);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_lifecycle_and_listing() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "laptop").await;
    let (session_id, session_token) =
        create_session(&channel, &device, "Architecture review").await;
    assert!(!session_token.is_empty());

    let mut sessions = SessionServiceClient::new(channel.clone());
    let got = sessions
        .get_session(request(
            proto::GetSessionRequest {
                session_id: session_id.clone(),
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.title, "Architecture review");
    assert_eq!(got.status, proto::SessionStatus::Created as i32);
    assert_eq!(got.transcript_count, 0);

    // A session token may read its own session but not another one.
    let own = sessions
        .get_session(request(
            proto::GetSessionRequest {
                session_id: session_id.clone(),
            },
            &session_token,
        ))
        .await
        .expect("session token can read its own session");
    assert_eq!(own.into_inner().id, session_id);

    let (other_id, _) = create_session(&channel, &device, "Other").await;
    let err = sessions
        .get_session(request(
            proto::GetSessionRequest {
                session_id: other_id,
            },
            &session_token,
        ))
        .await
        .expect_err("session token must not read another session");
    assert_eq!(err.code(), Code::Unauthenticated);

    let started = sessions
        .start_session(request(
            proto::StartSessionRequest {
                session_id: session_id.clone(),
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(started.status, proto::SessionStatus::Active as i32);
    assert!(started.started_at.is_some());

    let list = sessions
        .list_sessions(request(
            proto::ListSessionsRequest {
                limit: 50,
                active_only: true,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(list.sessions.iter().any(|s| s.id == session_id));

    let ended = sessions
        .end_session(request(
            proto::EndSessionRequest {
                session_id: session_id.clone(),
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    let ended_session = ended.session.unwrap();
    assert_eq!(ended_session.status, proto::SessionStatus::Completed as i32);
    assert!(ended_session.ended_at.is_some());

    // A completed session drops out of the active list.
    let list = sessions
        .list_sessions(request(
            proto::ListSessionsRequest {
                limit: 50,
                active_only: true,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!list.sessions.iter().any(|s| s.id == session_id));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_is_idempotent_and_sequenced() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "laptop").await;
    let (session_id, session_token) = create_session(&channel, &device, "Daily").await;
    let mut transcripts = TranscriptServiceClient::new(channel.clone());

    let first_id = new_uuid();
    let first = transcripts
        .publish_transcript(request(
            event(
                &session_id,
                &first_id,
                "them",
                "The migration window is Tuesday.",
            ),
            &session_token,
        ))
        .await
        .expect("publish with session token")
        .into_inner();
    assert_eq!(first.sequence_number, 1);
    assert!(!first.duplicate);

    // Retrying the same event_id must not create a second row.
    let retry = transcripts
        .publish_transcript(request(
            event(
                &session_id,
                &first_id,
                "them",
                "The migration window is Tuesday.",
            ),
            &session_token,
        ))
        .await
        .expect("retry")
        .into_inner();
    assert!(retry.duplicate);
    assert_eq!(retry.sequence_number, 1);

    let second = transcripts
        .publish_transcript(request(
            event(&session_id, &new_uuid(), "me", "We need security sign-off."),
            &session_token,
        ))
        .await
        .expect("publish")
        .into_inner();
    assert_eq!(second.sequence_number, 2);

    let sync = transcripts
        .sync_transcripts(request(
            proto::SyncTranscriptsRequest {
                session_id: session_id.clone(),
                from_sequence: 0,
                limit: 10,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sync.events.len(), 2);
    assert_eq!(sync.events[0].content, "The migration window is Tuesday.");
    assert_eq!(sync.events[0].speaker, "them");
    assert_eq!(sync.events[1].sequence_number, 2);
    assert_eq!(sync.latest_sequence, 2);
    assert!(!sync.has_more);

    // First committed line activates the session automatically.
    let mut sessions = SessionServiceClient::new(channel);
    let session = sessions
        .get_session(request(
            proto::GetSessionRequest {
                session_id: session_id.clone(),
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(session.status, proto::SessionStatus::Active as i32);
    assert_eq!(session.transcript_count, 2);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_token_cannot_publish_into_another_session() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "laptop").await;
    let (a, token_a) = create_session(&channel, &device, "A").await;
    let (b, _) = create_session(&channel, &device, "B").await;
    let mut transcripts = TranscriptServiceClient::new(channel);

    transcripts
        .publish_transcript(request(event(&a, &new_uuid(), "me", "in A"), &token_a))
        .await
        .expect("publish into own session");

    let err = transcripts
        .publish_transcript(request(event(&b, &new_uuid(), "me", "in B"), &token_a))
        .await
        .expect_err("cross-session publish must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_after_end_is_rejected() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "laptop").await;
    let (session_id, session_token) = create_session(&channel, &device, "Ending").await;

    SessionServiceClient::new(channel.clone())
        .end_session(request(
            proto::EndSessionRequest {
                session_id: session_id.clone(),
            },
            &device,
        ))
        .await
        .expect("end session");

    let mut transcripts = TranscriptServiceClient::new(channel);

    // Ending the session revokes its publish credential outright.
    let err = transcripts
        .publish_transcript(request(
            event(&session_id, &new_uuid(), "me", "late line"),
            &session_token,
        ))
        .await
        .expect_err("revoked session token must fail");
    assert_eq!(err.code(), Code::Unauthenticated);

    // A still-valid device token is told the session is over, not "not found".
    let err = transcripts
        .publish_transcript(request(
            event(&session_id, &new_uuid(), "me", "late line"),
            &device,
        ))
        .await
        .expect_err("publish into a completed session must fail");
    assert_eq!(err.code(), Code::FailedPrecondition);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_replays_backlog_then_delivers_live_events() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, session_token) = create_session(&channel, &device, "Live").await;
    let mut transcripts = TranscriptServiceClient::new(channel.clone());

    for line in ["first", "second"] {
        transcripts
            .publish_transcript(request(
                event(&session_id, &new_uuid(), "them", line),
                &session_token,
            ))
            .await
            .unwrap();
    }

    let mut stream = transcripts
        .stream_transcripts(request(
            proto::StreamTranscriptsRequest {
                session_id: session_id.clone(),
                from_sequence: 0,
            },
            &device,
        ))
        .await
        .expect("open stream")
        .into_inner();

    for expected in ["first", "second"] {
        let message = tokio::time::timeout(Duration::from_secs(5), stream.message())
            .await
            .expect("backlog timeout")
            .expect("stream error")
            .expect("stream closed");
        assert_eq!(message.content, expected);
    }

    // A live publish arrives without reconnecting.
    transcripts
        .publish_transcript(request(
            event(&session_id, &new_uuid(), "me", "third"),
            &session_token,
        ))
        .await
        .unwrap();
    let live = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .expect("live timeout")
        .expect("stream error")
        .expect("stream closed");
    assert_eq!(live.content, "third");
    assert_eq!(live.sequence_number, 3);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_resumes_from_a_sequence_number() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (session_id, session_token) = create_session(&channel, &device, "Resume").await;
    let mut transcripts = TranscriptServiceClient::new(channel.clone());

    for line in ["one", "two", "three"] {
        transcripts
            .publish_transcript(request(
                event(&session_id, &new_uuid(), "them", line),
                &session_token,
            ))
            .await
            .unwrap();
    }

    let mut stream = transcripts
        .stream_transcripts(request(
            proto::StreamTranscriptsRequest {
                session_id: session_id.clone(),
                from_sequence: 2,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();

    let resume = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(resume.content, "three");
    assert_eq!(resume.sequence_number, 3);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_help_flag_is_stored_with_the_session() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "phone").await;
    let (interview_id, _) = create_session_with_ai(&channel, &device, "Interview", true).await;
    let (plain_id, _) = create_session(&channel, &device, "Plain").await;

    let mut sessions = SessionServiceClient::new(channel);
    let interview = sessions
        .get_session(request(
            proto::GetSessionRequest {
                session_id: interview_id,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(interview.ai_help);

    let plain = sessions
        .get_session(request(
            proto::GetSessionRequest {
                session_id: plain_id,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!plain.ai_help);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn transcripts_survive_a_server_restart() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "laptop").await;
    let (session_id, session_token) = create_session(&channel, &device, "Durable").await;
    let mut transcripts = TranscriptServiceClient::new(channel.clone());
    for line in ["before restart", "still here"] {
        transcripts
            .publish_transcript(request(
                event(&session_id, &new_uuid(), "them", line),
                &session_token,
            ))
            .await
            .unwrap();
    }

    let restarted = server.restart().await;
    let channel = restarted.channel().await;
    let device = pair(&channel, "laptop-again").await;
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
        .expect("sync after restart")
        .into_inner();
    assert_eq!(sync.events.len(), 2);
    assert_eq!(sync.events[0].content, "before restart");
    assert_eq!(sync.events[1].content, "still here");
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_pages_through_a_long_transcript() {
    let Some(server) = TestServer::start().await else {
        eprintln!("{}", skip_reason());
        return;
    };
    let channel = server.channel().await;
    let device = pair(&channel, "laptop").await;
    let (session_id, session_token) = create_session(&channel, &device, "Long").await;
    let mut transcripts = TranscriptServiceClient::new(channel.clone());
    for n in 0..7 {
        transcripts
            .publish_transcript(request(
                event(&session_id, &new_uuid(), "them", &format!("line {n}")),
                &session_token,
            ))
            .await
            .unwrap();
    }

    let page = transcripts
        .sync_transcripts(request(
            proto::SyncTranscriptsRequest {
                session_id: session_id.clone(),
                from_sequence: 0,
                limit: 3,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.events.len(), 3);
    assert!(page.has_more);
    assert_eq!(page.latest_sequence, 7);

    let rest = transcripts
        .sync_transcripts(request(
            proto::SyncTranscriptsRequest {
                session_id: session_id.clone(),
                from_sequence: 3,
                limit: 100,
            },
            &device,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(rest.events.len(), 4);
    assert!(!rest.has_more);
    assert_eq!(rest.events.last().unwrap().content, "line 6");
    server.stop().await;
}
