//! Email the meeting notes when a session ends.
//!
//! Off unless the operator configured it: the notes are the user's transcript,
//! so the only default is "send nothing". Configured entirely by environment
//! (`WHEEZPER_EMAIL_TO`, `_FROM`, `_USER`, `_PASS`, `_RELAY`) so no secret ever
//! lives in the database or the app. A send failure is logged and never fails
//! `EndSession`: the meeting is already over and the transcript is stored.

use crate::error::{Error, Result};
use crate::session::SessionRow;
use crate::transcript::StoredEvent;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};

#[derive(Clone, Debug)]
pub struct Email {
    to: Vec<String>,
    from: String,
    user: String,
    pass: String,
    relay: String,
    /// STARTTLS on 587 (Brevo, most providers) or implicit TLS on 465.
    implicit_tls: bool,
}

impl Email {
    pub fn from_env() -> Option<Self> {
        let to: Vec<String> = std::env::var("WHEEZPER_EMAIL_TO")
            .ok()?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let user = std::env::var("WHEEZPER_EMAIL_USER").ok()?;
        let pass = std::env::var("WHEEZPER_EMAIL_PASS").ok()?;
        if to.is_empty() || user.is_empty() || pass.is_empty() {
            return None;
        }
        let from = std::env::var("WHEEZPER_EMAIL_FROM")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| user.clone());
        let relay = std::env::var("WHEEZPER_EMAIL_RELAY")
            .unwrap_or_else(|_| "smtp-relay.brevo.com".to_string());
        let implicit_tls = std::env::var("WHEEZPER_EMAIL_TLS")
            .map(|v| v.eq_ignore_ascii_case("implicit"))
            .unwrap_or(false);
        Some(Self { to, from, user, pass, relay, implicit_tls })
    }

    /// Relay name for the startup log (the host only, never the credentials).
    pub fn relay_name(&self) -> &str {
        &self.relay
    }

    /// Send one message. Blocking SMTP runs on a blocking thread.
    pub async fn send(&self, subject: String, body: String) -> Result<()> {
        if self.to.is_empty() {
            return Ok(());
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.send_blocking(subject, body))
            .await
            .map_err(|e| Error::provider(format!("email task failed: {e}")))?
    }

    fn send_blocking(self, subject: String, body: String) -> Result<()> {
        let mut builder = Message::builder()
            .from(
                self.from
                    .parse()
                    .map_err(|e| Error::provider(format!("bad WHEEZPER_EMAIL_FROM: {e}")))?,
            )
            .subject(subject)
            .header(ContentType::TEXT_PLAIN);
        for addr in &self.to {
            builder = builder.to(addr
                .parse()
                .map_err(|e| Error::provider(format!("bad WHEEZPER_EMAIL_TO: {e}")))?);
        }
        let message = builder
            .body(body)
            .map_err(|e| Error::provider(format!("could not build email: {e}")))?;

        let creds = Credentials::new(self.user.clone(), self.pass.clone());
        let transport = if self.implicit_tls {
            SmtpTransport::relay(&self.relay)
        } else {
            SmtpTransport::starttls_relay(&self.relay)
        }
        .map_err(|e| Error::provider(format!("bad smtp relay {}: {e}", self.relay)))?
        .credentials(creds)
        .build();
        transport
            .send(&message)
            .map(|_| ())
            .map_err(|e| Error::provider(format!("smtp send failed: {e}")))
    }
}

/// Markdown notes for one finished meeting, same row shape as the laptop's
/// `notes/YYYY-MM-DD.md` so a mail and a local file read the same.
pub fn render_notes(session: &SessionRow, entries: &[StoredEvent]) -> String {
    let started = session
        .started_at
        .unwrap_or(session.created_at)
        .with_timezone(&chrono::Local);
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", session.title));
    out.push_str(&format!(
        "{} · {} line(s) · session {}\n\n",
        started.format("%Y-%m-%d %H:%M"),
        entries.len(),
        session.id
    ));
    for entry in entries {
        let time = entry.started_at.with_timezone(&chrono::Local).format("%H:%M:%S");
        match entry.speaker.trim() {
            "" => out.push_str(&format!("- [{}] {}\n", time, entry.content.trim())),
            speaker => out.push_str(&format!(
                "- [{}] **{}:** {}\n",
                time,
                speaker,
                entry.content.trim()
            )),
        }
    }
    out
}

/// Fetch the transcript and mail it. Best-effort: the caller logs the error.
pub async fn send_notes(state: &crate::grpc::AppState, session: &SessionRow) -> Result<()> {
    let Some(email) = state.email.as_ref() else {
        return Ok(());
    };
    // A meeting is bounded; 20k rows is plenty and keeps the query honest.
    let entries = state.transcripts.recent(session.id, 20_000).await?;
    let subject = format!("wheezper: {} — {} line(s)", session.title, entries.len());
    email.send(subject, render_notes(session, &entries)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn session() -> SessionRow {
        SessionRow {
            id: Uuid::nil(),
            title: "Deployment review".into(),
            status: "completed".into(),
            started_at: Some(chrono::Utc.with_ymd_and_hms(2026, 1, 2, 9, 0, 0).unwrap()),
            ended_at: Some(chrono::Utc.with_ymd_and_hms(2026, 1, 2, 9, 30, 0).unwrap()),
            created_at: chrono::Utc.with_ymd_and_hms(2026, 1, 2, 9, 0, 0).unwrap(),
            transcript_count: 2,
            ai_help: false,
            ai_context: String::new(),
            ai_instructions: String::new(),
        }
    }

    fn entry(seq: i64, speaker: &str, content: &str) -> StoredEvent {
        StoredEvent {
            id: Uuid::new_v4(),
            session_id: Uuid::nil(),
            event_id: Uuid::new_v4(),
            sequence_number: seq,
            speaker: speaker.into(),
            content: content.into(),
            started_at: chrono::Utc.with_ymd_and_hms(2026, 1, 2, 9, 0, seq as u32).unwrap(),
            received_at: chrono::Utc.with_ymd_and_hms(2026, 1, 2, 9, 0, seq as u32).unwrap(),
        }
    }

    #[test]
    fn notes_are_the_same_rows_the_laptop_writes() {
        let notes = render_notes(
            &session(),
            &[entry(1, "them", "Deploy on Tuesday."), entry(2, "", "ok")],
        );
        assert!(notes.starts_with("# Deployment review\n"));
        assert!(notes.contains("2 line(s)"));
        assert!(notes.contains("**them:** Deploy on Tuesday."));
        assert!(notes.contains("ok"));
    }

    #[test]
    fn email_is_off_without_configuration() {
        // The default deployment must never mail a transcript anywhere.
        assert!(Email::from_env().is_none() || std::env::var("WHEEZPER_EMAIL_TO").is_ok());
    }
}
