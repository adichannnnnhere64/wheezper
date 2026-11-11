//! Bounded meeting context for a question.
//!
//! MVP retrieval is "recent transcript window + PostgreSQL full-text matches
//! for older lines". That is enough to answer questions about an ongoing
//! meeting without embeddings, and it is deterministic (no vector index to
//! keep in sync). Swap in embeddings later behind this same `MeetingContext`.

use crate::transcript::StoredEvent;
use crate::transcript::service::TranscriptService;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Recent window carried verbatim in every prompt.
const RECENT_ENTRIES: i64 = 250;
/// Interview answers follow the last question, not the whole meeting. A small
/// model on a laptop-class box has to prefill this context before it answers,
/// so a long interview must not turn into a 30 s prompt.
const INTERVIEW_RECENT_ENTRIES: i64 = 12;
/// Older full-text matches added when the question has keywords.
const OLDER_MATCHES: i64 = 20;
/// Hard cap on transcript characters sent to the model.
const MAX_CONTEXT_CHARS: usize = 12_000;
const MAX_EARLIER_CHARS: usize = 2_000;

#[derive(Clone, Debug)]
pub struct ContextEntry {
    pub sequence_number: i64,
    pub speaker: String,
    pub content: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct MeetingContext {
    pub session_id: Uuid,
    pub title: String,
    pub recent: Vec<ContextEntry>,
    pub earlier: Vec<ContextEntry>,
    pub total_entries: i64,
}

/// What kind of answer the caller wants.
///
/// A meeting session answers *about* the transcript and must never add outside
/// facts. A session created with `ai_help` is an interview: the far side asks a
/// question and the user needs an answer to say out loud, so general technical
/// knowledge is allowed (personal claims still are not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Meeting,
    Interview,
}

impl Mode {
    /// `ai_help` is set when the session was created from the phone's
    /// interview-assist dialog.
    pub fn for_session(ai_help: bool) -> Self {
        if ai_help { Self::Interview } else { Self::Meeting }
    }

    /// How much transcript the prompt carries.
    pub fn recent_entries(self) -> i64 {
        match self {
            Self::Meeting => RECENT_ENTRIES,
            Self::Interview => INTERVIEW_RECENT_ENTRIES,
        }
    }

    /// Older full-text matches: useful for "what was decided last hour", noise
    /// for "answer the question you just heard".
    pub fn searches_older_lines(self) -> bool {
        matches!(self, Self::Meeting)
    }
}

impl MeetingContext {
    /// Assemble the two prompt messages. The system prompt forbids inventing
    /// meeting content: an answer is a suggestion, never a transcript edit.
    pub fn messages(&self, question: &str, mode: Mode) -> (String, String) {
        let mut user = String::new();
        user.push_str(&format!("Meeting: {}\n", self.title));
        if self.total_entries == 0 {
            user.push_str("The meeting has no transcript yet.\n");
        } else {
            user.push_str(&format!(
                "Transcript ({} entries so far{}):\n",
                self.total_entries,
                if self.earlier.is_empty() {
                    ""
                } else {
                    ", relevant excerpts first"
                }
            ));
            if !self.earlier.is_empty() {
                user.push_str("Relevant earlier excerpts:\n");
                let text = render_entries(&self.earlier, MAX_EARLIER_CHARS);
                user.push_str(&text);
                user.push('\n');
            }
            user.push_str("Most recent transcript:\n");
            user.push_str(&render_entries(
                &self.recent,
                MAX_CONTEXT_CHARS - MAX_EARLIER_CHARS,
            ));
        }
        match mode {
            Mode::Meeting => {
                user.push_str("\nQuestion: ");
                user.push_str(question.trim());
                user.push_str(
                    "\n\nAnswer the question using the transcript above. If the transcript does not \
                     contain the answer, say so explicitly. Never claim something was said unless it \
                     appears in the transcript. This answer is a suggestion shown to the user; it is \
                     not part of the meeting record.",
                );
                let system = "You are a concise meeting assistant embedded in a live transcription \
                              app. You answer questions about the meeting transcript you are given. \
                              You never invent statements, decisions, names or numbers that are not \
                              in the transcript. Keep answers under 120 words and lead with the direct \
                              answer."
                    .to_string();
                (system, user)
            }
            Mode::Interview => {
                user.push_str("\nThe interviewer just said: ");
                user.push_str(question.trim());
                user.push_str("\n\nAnswer it as the candidate would out loud.");
                let system = "You are helping the candidate in a live interview. `them` is the \
                              interviewer, `me` is the candidate. Answer the interviewer's latest \
                              line as the candidate: one conversational sentence of 10-20 words, \
                              accurate and concrete, no preamble, no lists, no \"as an AI\". Use the \
                              transcript for context and your own general technical knowledge for \
                              the substance; if the transcript already shows relevant experience, \
                              prefer it. Never invent personal facts, employers, dates or experience \
                              that are not in the transcript."
                    .to_string();
                (system, user)
            }
        }
    }
}

fn render_entries(entries: &[ContextEntry], max_chars: usize) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let time = e.started_at.format("%H:%M:%S");
            match e.speaker.trim() {
                "" => format!("[{time}] {}", e.content),
                speaker => format!("[{time}] {speaker}: {}", e.content),
            }
        })
        .collect();
    // Keep the newest lines when the budget is exceeded.
    let mut total = 0usize;
    let mut first = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        total += line.len() + 1;
        if total > max_chars {
            break;
        }
        first = i;
    }
    if first > 0 {
        lines.drain(..first);
    }
    if lines.is_empty() && !entries.is_empty() {
        // One enormous line still has to fit: keep its head, not nothing.
        let newest = render_entries(&entries[entries.len() - 1..], usize::MAX);
        let newest = newest.trim_end();
        lines.push(newest.chars().take(max_chars).collect());
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Load the context for one question.
pub async fn build(
    transcripts: &TranscriptService,
    session_id: Uuid,
    title: &str,
    question: &str,
    mode: Mode,
) -> crate::error::Result<MeetingContext> {
    let recent = transcripts.recent(session_id, mode.recent_entries()).await?;
    let total_entries =
        crate::transcript::repository::count(transcripts.pool(), session_id).await?;

    // Older lines only matter when the recent window does not cover the whole
    // meeting; searching for every question would cost a GIN lookup for free.
    let earlier = if mode.searches_older_lines() && total_entries > recent.len() as i64 {
        let before = recent
            .first()
            .map(|e| e.sequence_number)
            .unwrap_or(i64::MAX);
        transcripts
            .search(session_id, question, before, OLDER_MATCHES)
            .await?
    } else {
        Vec::new()
    };

    Ok(MeetingContext {
        session_id,
        title: title.to_string(),
        recent: recent.into_iter().map(to_entry).collect(),
        earlier: earlier.into_iter().map(to_entry).collect(),
        total_entries,
    })
}

fn to_entry(event: StoredEvent) -> ContextEntry {
    ContextEntry {
        sequence_number: event.sequence_number,
        speaker: event.speaker,
        content: event.content,
        started_at: event.started_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(seq: i64, speaker: &str, content: &str) -> ContextEntry {
        ContextEntry {
            sequence_number: seq,
            speaker: speaker.to_string(),
            content: content.to_string(),
            started_at: Utc
                .with_ymd_and_hms(2026, 11, 11, 2, 0, seq as u32)
                .unwrap(),
        }
    }

    #[test]
    fn renders_speaker_and_time_per_line() {
        let text = render_entries(&[entry(1, "them", "hello"), entry(2, "", "aside")], 1000);
        assert!(text.contains("[02:00:01] them: hello"));
        assert!(text.contains("[02:00:02] aside"));
    }

    #[test]
    fn keeps_the_newest_lines_when_over_budget() {
        let entries = vec![
            entry(1, "them", &"old ".repeat(30)),
            entry(2, "me", "newest line"),
        ];
        let text = render_entries(&entries, 40);
        assert!(text.contains("newest line"));
        assert!(!text.contains("old old"));
    }

    #[test]
    fn prompt_separates_earlier_excerpts_and_forbids_invention() {
        let ctx = MeetingContext {
            session_id: Uuid::nil(),
            title: "Review".into(),
            recent: vec![entry(5, "them", "deploy on tuesday")],
            earlier: vec![entry(1, "me", "budget is fixed")],
            total_entries: 9,
        };
        let (system, user) = ctx.messages("what about the budget?", Mode::Meeting);
        assert!(user.contains("Relevant earlier excerpts:"));
        assert!(user.contains("budget is fixed"));
        assert!(user.contains("Most recent transcript:"));
        assert!(user.contains("deploy on tuesday"));
        assert!(user.contains("Question: what about the budget?"));
        assert!(system.contains("never invent"));
    }

    #[test]
    fn interview_mode_answers_the_far_side_from_general_knowledge() {
        let ctx = MeetingContext {
            session_id: Uuid::nil(),
            title: "Interview".into(),
            recent: vec![entry(1, "them", "What is Java?")],
            earlier: vec![],
            total_entries: 1,
        };
        let (system, user) = ctx.messages("What is Java?", Mode::Interview);
        assert!(user.contains("interviewer just said: What is Java?"));
        assert!(user.contains("Answer it as the candidate"));
        assert!(system.contains("general technical knowledge"));
        assert!(system.contains("Never invent personal facts"));
        // Meeting mode must never invite outside knowledge: it is a record.
        let (meeting_system, _) = ctx.messages("What is Java?", Mode::Meeting);
        assert!(!meeting_system.contains("general technical knowledge"));
    }

    #[test]
    fn ai_help_sessions_use_interview_mode() {
        assert_eq!(Mode::for_session(true), Mode::Interview);
        assert_eq!(Mode::for_session(false), Mode::Meeting);
        // A long interview must not ship the whole meeting to a small model.
        assert!(Mode::Interview.recent_entries() < Mode::Meeting.recent_entries());
        assert!(!Mode::Interview.searches_older_lines());
        assert!(Mode::Meeting.searches_older_lines());
    }

    #[test]
    fn empty_meeting_is_stated_not_implied() {
        let ctx = MeetingContext {
            session_id: Uuid::nil(),
            title: "Empty".into(),
            recent: vec![],
            earlier: vec![],
            total_entries: 0,
        };
        let (_, user) = ctx.messages("summarize", Mode::Meeting);
        assert!(user.contains("no transcript yet"));
    }

    #[test]
    fn oversized_single_line_is_truncated_not_dropped() {
        let long = "x".repeat(500);
        let text = render_entries(&[entry(1, "them", &long)], 50);
        assert!(text.contains('x'), "the only line must not disappear");
        assert!(text.trim_end().chars().count() <= 50);
    }
}
