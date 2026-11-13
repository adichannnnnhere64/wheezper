//! Bounded meeting context for a question.
//!
//! MVP retrieval is "recent transcript window + PostgreSQL full-text matches
//! for older lines + the answers already given in this meeting". That is enough
//! to answer questions about an ongoing meeting without embeddings, and it is
//! deterministic (no vector index to keep in sync). Swap in embeddings later
//! behind this same `MeetingContext`; `Budget` stays the size knob.

use crate::transcript::StoredEvent;
use crate::transcript::service::TranscriptService;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Recent window carried verbatim in every prompt.
const RECENT_ENTRIES: i64 = 250;
/// Interview answers follow the last question, not the whole meeting. A small
/// model on a laptop-class box has to prefill this context before it answers,
/// so a long interview must not turn into a 30 s prompt. A cloud model is not
/// prefill-bound, so it gets the whole meeting instead — see `Budget`.
const INTERVIEW_RECENT_ENTRIES: i64 = 12;
/// Older full-text matches added when the question has keywords.
const OLDER_MATCHES: i64 = 20;
/// Hard cap on transcript characters sent to the model.
const MAX_CONTEXT_CHARS: usize = 12_000;
/// Cloud models have the context window for the whole meeting; 12k chars is
/// only the local model's prefill budget.
const CLOUD_CONTEXT_CHARS: usize = 48_000;
const MAX_EARLIER_CHARS: usize = 2_000;
/// Cap on the "what you already said" block (previous Q&A pairs).
const MAX_PREVIOUS_CHARS: usize = 8_000;
/// The user's session brief and answer guidance are prefilled on every request,
/// so they are bounded here as well as at the RPC edge.
const MAX_BRIEF_CHARS: usize = 2_000;
const MAX_INSTRUCTIONS_CHARS: usize = 600;

#[derive(Clone, Debug)]
pub struct ContextEntry {
    pub sequence_number: i64,
    pub speaker: String,
    pub content: String,
    pub started_at: DateTime<Utc>,
}

/// How much context one request carries.
///
/// Two tiers, for the same reason the transcript has two: prefill, not decoding,
/// is the latency on a small box. A local model answering an interview gets the
/// last few lines plus the last few answers; a cloud model (or a meeting
/// assistant) gets the whole meeting and its earlier answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub mode: Mode,
    pub recent_entries: i64,
    pub searches_older_lines: bool,
    /// Previous question/answer pairs of this meeting carried in the prompt.
    pub previous_answers: i64,
    pub max_chars: usize,
    /// The user asked for detail (double-tap on the row), not the automatic
    /// one-liner. Only the answer's shape changes, never the fact rules.
    pub detail: bool,
}

impl Budget {
    pub fn with_detail(mut self, detail: bool) -> Self {
        self.detail = detail;
        self
    }

    pub fn for_request(mode: Mode, cloud: bool) -> Self {
        let short = mode == Mode::Interview && !cloud;
        Self {
            mode,
            recent_entries: if short {
                INTERVIEW_RECENT_ENTRIES
            } else {
                RECENT_ENTRIES
            },
            searches_older_lines: !short,
            previous_answers: if cloud { 20 } else { 4 },
            max_chars: if cloud {
                CLOUD_CONTEXT_CHARS
            } else {
                MAX_CONTEXT_CHARS
            },
            detail: false,
        }
    }
}

/// One earlier answer of this meeting, as carried in the prompt.
#[derive(Clone, Debug)]
pub struct PreviousAnswer {
    pub question: String,
    pub answer: String,
}

#[derive(Clone, Debug)]
pub struct MeetingContext {
    pub session_id: Uuid,
    pub title: String,
    /// What the user wrote this meeting is about (may be empty).
    pub context: String,
    /// Answer guidance the user asked for (may be empty).
    pub instructions: String,
    /// "About me" text the user typed in Settings; background for answers.
    pub about_me: String,
    /// What a person (the operator, or whoever invited this device) recorded
    /// about the user in their profile. Unlike `about_me`, this is written for
    /// the user, not by them, and is the authority on personal facts — salary,
    /// notice period, years of experience — that a model must not invent.
    pub facts: Option<ProfileFacts>,
    /// Curated answers a person wrote for this user, best match first.
    pub cards: Vec<AnswerCard>,
    pub recent: Vec<ContextEntry>,
    pub earlier: Vec<ContextEntry>,
    pub previous: Vec<PreviousAnswer>,
    pub total_entries: i64,
}

/// What kind of answer the caller wants.
///
/// A meeting session answers *about* the transcript and must never add outside
/// facts. A session created with `ai_help` is an interview: the far side asks a
/// question and the user needs an answer to say out loud, so general technical
/// knowledge is allowed (personal claims still are not).
///
/// A curated answer card is a question/answer pair a person wrote deliberately.
/// It outranks the model: when it matches on at least `MIN_CARD_HITS` distinct
/// terms the server answers from the card and never asks the model, because the
/// small models here invent details (a Kubernetes question once pulled a
/// Kubernetes-with-Helm answer out of thin air, and a Gemini card for a
/// Kubernetes question).
#[derive(Clone, Debug)]
pub struct AnswerCard {
    pub question: String,
    pub answer: String,
    pub rank: f64,
    /// How many distinct query terms the card actually matched. A single generic
    /// term ("experience") is not a match.
    pub hits: i64,
}

/// Distinct matched terms a card needs before it may answer on its own.
pub const MIN_CARD_HITS: i64 = 2;

/// A curated record of the person this device answers for.
///
/// Written by the operator (or whoever created the invite), never by the model.
/// Kept separate from the transcript on purpose: it is the authority on personal
/// facts, so an answer that contradicts it is wrong even when it sounds fluent.
#[derive(Clone, Debug)]
pub struct ProfileFacts {
    pub summary: String,
}

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
}

impl MeetingContext {
    /// Assemble the two prompt messages. The system prompt forbids inventing
    /// meeting content: an answer is a suggestion, never a transcript edit.
    pub fn messages(&self, question: &str, budget: Budget) -> (String, String) {
        let mode = budget.mode;
        let mut user = String::new();
        user.push_str(&format!("Meeting: {}\n", self.title));

        // The brief is the user's own words about what this session is. It is
        // context, never evidence: the prompt still refuses to claim anything
        // that is not in the transcript (meeting) or the knowledgebase
        // (interview personal facts).
        if !self.context.trim().is_empty() {
            user.push_str("What this session is about (written by the user):\n");
            user.push_str(self.context.trim());
            user.push_str("\n\n");
        }

        // The profile record outranks both the model and "about me": a person
        // curated these facts (salary, notice period, years), so an answer that
        // contradicts them is wrong no matter how fluent it sounds.
        if let Some(facts) = self.facts.as_ref().filter(|f| !f.summary.trim().is_empty()) {
            user.push_str(
                "Facts recorded about me by the person who set this up (these are the \
                 truth — use these numbers and claims exactly, and never state a \
                 personal fact that is not in here):\n",
            );
            user.push_str(facts.summary.trim());
            user.push_str("\n\n");
        }

        // "About me" from the phone's settings. The user wrote it about
        // themselves, so it may shape the answer — but it is not a transcript,
        // and the mode's rules about inventing facts still apply.
        if !self.about_me.trim().is_empty() {
            user.push_str("About me, from my own settings (use it for my experience and \
                          preferences; never invent anything beyond it):\n");
            user.push_str(self.about_me.trim());
            user.push_str("\n\n");
        }

        // Curated answers beat the model. They are the user's own prepared
        // wording, so they are quoted as-is rather than paraphrased.
        if !self.cards.is_empty() {
            user.push_str(
                "Prepared answers for questions like this one, written by the user \
                 (prefer the closest one and keep its facts and numbers; do not \
                 mix details from different prepared answers):\n",
            );
            for card in &self.cards {
                user.push_str(&format!("Q: {}\nA: {}\n\n", card.question.trim(), card.answer.trim()));
            }
        }

        // Answers already given are part of the assistant's memory: a question
        // asked twice, or a follow-up to one of our own answers, must not get
        // a contradicting answer just because the model cannot see the last one.
        if !self.previous.is_empty() {
            user.push_str("What you already answered in this meeting (stay consistent, do not repeat):\n");
            user.push_str(&render_previous(&self.previous, MAX_PREVIOUS_CHARS));
            user.push('\n');
        }

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
                budget.max_chars.saturating_sub(MAX_EARLIER_CHARS),
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
                              answer. Always answer in English, even when the transcript is in \
                              another language."
                    .to_string();
                (with_guidance(system, &self.instructions), user)
            }
            Mode::Interview => {
                user.push_str("\nThe interviewer just said: ");
                user.push_str(question.trim());
                user.push_str("\n\nAnswer it as the candidate would out loud.");
                let system = if budget.detail {
                    // The user tapped the row for more: the automatic 10-20
                    // word line is the wrong shape now, but the rules about
                    // inventing facts are not negotiable.
                    "You are helping the candidate in a live interview. `them` is the \
                     interviewer, `me` is the candidate. Explain the interviewer's latest line \
                     as the candidate: 2-4 sentences of concrete technical detail — what it is, \
                     the trade-offs, and a brief example. No preamble, no lists, no \"as an AI\". \
                     Use the transcript for context and your own general technical knowledge for \
                     the substance; if the transcript already shows relevant experience, prefer \
                     it. Never invent personal facts, employers, dates or experience that are not \
                     in the transcript. Always answer in English, even when the transcript is \
                     Tagalog or mixed."
                } else {
                    "You are helping the candidate in a live interview. `them` is the \
                     interviewer, `me` is the candidate. Answer the interviewer's latest line as \
                     the candidate: one conversational sentence of 10-20 words, accurate and \
                     concrete, no preamble, no lists, no \"as an AI\". Use the transcript for \
                     context and your own general technical knowledge for the substance; if the \
                     transcript already shows relevant experience, prefer it. Never invent \
                     personal facts, employers, dates or experience that are not in the \
                     transcript. Always answer in English, even when the transcript is Tagalog \
                     or mixed."
                }
                .to_string();
                (with_guidance(system, &self.instructions), user)
            }
        }
    }

    /// The best prepared answer, when it matched strongly enough to stand alone.
    ///
    /// One hit is not a match: a card on "Kubernetes experience" would otherwise
    /// answer every question that happens to contain "experience".
    pub fn card_answer(&self) -> Option<&AnswerCard> {
        self.cards
            .iter()
            .find(|card| card.hits >= MIN_CARD_HITS && !card.answer.trim().is_empty())
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
    context: &str,
    instructions: &str,
    about_me: &str,
    question: &str,
    profile_id: Option<Uuid>,
    budget: Budget,
) -> crate::error::Result<MeetingContext> {
    let recent = transcripts.recent(session_id, budget.recent_entries).await?;
    let total_entries =
        crate::transcript::repository::count(transcripts.pool(), session_id).await?;

    // Older lines only matter when the recent window does not cover the whole
    // meeting; searching for every question would cost a GIN lookup for free.
    let earlier = if budget.searches_older_lines && total_entries > recent.len() as i64 {
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

    let previous = if budget.previous_answers > 0 {
        crate::assistant::repository::previous_answers(
            transcripts.pool(),
            session_id,
            budget.previous_answers,
        )
        .await?
        .into_iter()
        .map(|(question, answer)| PreviousAnswer { question, answer })
        .collect()
    } else {
        Vec::new()
    };

    let cards = load_cards(transcripts.pool(), question, profile_id)
        .await
        .unwrap_or_default();
    let facts = load_facts(transcripts.pool(), profile_id).await.unwrap_or_default();

    Ok(MeetingContext {
        session_id,
        title: title.to_string(),
        context: bounded(context, MAX_BRIEF_CHARS),
        instructions: bounded(instructions, MAX_INSTRUCTIONS_CHARS),
        about_me: bounded(about_me, MAX_ABOUT_ME_CHARS),
        facts,
        cards,
        recent: recent.into_iter().map(to_entry).collect(),
        earlier: earlier.into_iter().map(to_entry).collect(),
        previous,
        total_entries,
    })
}

/// Up to four curated answer cards matching the question.
///
/// `plainto_tsquery` ANDs every lexeme, which matches nothing for a spoken
/// question ("can you just name me the components of an agent..."). Build an OR
/// query from the question's lexemes instead and rank by ts_rank.
///
/// The OR uses `to_tsquery('simple', ...)` because tsvector_to_array returns
/// stems already; `to_tsquery('english', ...)` stems them a second time
/// ('databas' -> 'databa') and silently matches nothing.
///
/// Cards shared by everyone plus this device's own profile cards.
async fn load_cards(
    pool: &sqlx::PgPool,
    question: &str,
    profile_id: Option<Uuid>,
) -> crate::error::Result<Vec<AnswerCard>> {
    let rows: Vec<(String, String, f64, i64)> = sqlx::query_as(
        "WITH q AS ( \
             SELECT to_tsvector('english', $1) AS qv \
         ), terms AS ( \
             SELECT unnest(tsvector_to_array(qv)) AS term FROM q \
         ), q2 AS ( \
             SELECT to_tsquery('simple', string_agg(term, ' | ')) AS tsq FROM terms \
         ) \
         SELECT c.question, c.answer, \
                ts_rank(to_tsvector('english', c.question || ' ' || c.keywords || ' ' || c.answer), \
                        q2.tsq)::float8 AS rank, \
                (SELECT count(*) FROM terms t \
                  WHERE to_tsvector('english', c.keywords) \
                        @@ to_tsquery('simple', t.term))::bigint AS hits \
         FROM answer_cards c, q2 \
         WHERE q2.tsq IS NOT NULL \
           AND (c.profile_id IS NULL OR c.profile_id = $2) \
           AND to_tsvector('english', c.question || ' ' || c.keywords || ' ' || c.answer) \
               @@ q2.tsq \
           AND to_tsvector('english', c.keywords) @@ q2.tsq \
         ORDER BY hits DESC, rank DESC \
         LIMIT 4",
    )
    .bind(question)
    .bind(profile_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(question, answer, rank, hits)| AnswerCard {
            question,
            answer,
            rank,
            hits,
        })
        .collect())
}

/// The profile record this device answers for, when it has one.
///
/// A device paired before profiles existed has `profile_id = NULL` and simply
/// gets no facts — the answer falls back to "about me" and the model.
async fn load_facts(
    pool: &sqlx::PgPool,
    profile_id: Option<Uuid>,
) -> crate::error::Result<Option<ProfileFacts>> {
    let Some(profile_id) = profile_id else {
        return Ok(None);
    };
    let row: Option<(String,)> =
        sqlx::query_as("SELECT summary FROM profiles WHERE id = $1")
            .bind(profile_id)
            .fetch_optional(pool)
            .await?;
    Ok(row
        .filter(|(summary,)| !summary.trim().is_empty())
        .map(|(summary,)| ProfileFacts { summary }))
}

/// Append the user's own answer guidance to a mode's system prompt. The rules
/// above it stay authoritative: guidance can ask for a tone, a length or a
/// grammar correction, it cannot license inventing meeting content.
fn with_guidance(mut system: String, instructions: &str) -> String {
    let instructions = instructions.trim();
    if instructions.is_empty() {
        return system;
    }
    system.push_str("\n\nThe user asked for this answer guidance: ");
    system.push_str(instructions);
    system.push_str("\nFollow it where it does not conflict with the rules above.");
    system
}

/// Trim and cap a user-written prompt field (defence in depth: the session
/// service already rejects over-long briefs).
/// "About me" is prefilled on every request, so it is capped like the brief:
/// a paragraph, not a document. The transcript still has to fit.
const MAX_ABOUT_ME_CHARS: usize = 2_000;

fn bounded(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

/// Render earlier answers, keeping the newest pairs when over budget.
fn render_previous(previous: &[PreviousAnswer], max_chars: usize) -> String {
    let mut lines: Vec<String> = previous
        .iter()
        .map(|p| format!("Q: {}\nA: {}", p.question.trim(), p.answer.trim()))
        .collect();
    let mut total = 0usize;
    let mut first = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        total += line.len() + 2;
        if total > max_chars {
            break;
        }
        first = i;
    }
    if first > 0 {
        lines.drain(..first);
    }
    if lines.is_empty() && !previous.is_empty() {
        let newest = &previous[previous.len() - 1];
        lines.push(format!("Q: {}", newest.question.trim()));
    }
    let mut out = lines.join("\n\n");
    out.push('\n');
    out
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

    fn ctx(recent: Vec<ContextEntry>, earlier: Vec<ContextEntry>, total: i64) -> MeetingContext {
        MeetingContext {
            session_id: Uuid::nil(),
            title: "Review".into(),
            context: String::new(),
            instructions: String::new(),
            about_me: String::new(),
            facts: None,
            cards: Vec::new(),
            recent,
            earlier,
            previous: Vec::new(),
            total_entries: total,
        }
    }

    #[test]
    fn a_prepared_answer_needs_two_matching_terms() {
        let mut meeting = ctx(Vec::new(), Vec::new(), 0);
        meeting.cards = vec![
            AnswerCard {
                question: "What does an agent loop look like?".into(),
                answer: "perceive, plan, act, reflect".into(),
                rank: 0.9,
                hits: 3,
            },
            AnswerCard {
                question: "Kubernetes experience".into(),
                answer: "three years of Helm charts".into(),
                rank: 0.1,
                hits: 1,
            },
        ];
        let best = meeting.card_answer().expect("the strong card answers");
        assert_eq!(best.answer, "perceive, plan, act, reflect");
    }

    #[test]
    fn one_generic_term_is_not_a_match() {
        let mut meeting = ctx(Vec::new(), Vec::new(), 0);
        meeting.cards = vec![AnswerCard {
            question: "Kubernetes experience".into(),
            answer: "three years of Helm charts".into(),
            rank: 0.1,
            hits: 1,
        }];
        assert!(meeting.card_answer().is_none());
    }

    #[test]
    fn prepared_answers_are_in_the_prompt_but_never_the_system_prompt() {
        let mut meeting = ctx(vec![entry(1, "them", "name the parts of an agent")], Vec::new(), 1);
        meeting.cards = vec![AnswerCard {
            question: "Components of an agent".into(),
            answer: "model, tools, memory and an orchestration loop".into(),
            rank: 0.8,
            hits: 2,
        }];
        let (system, user) =
            meeting.messages("name the parts of an agent", Budget::for_request(Mode::Interview, false));
        assert!(user.contains("Prepared answers"));
        assert!(user.contains("model, tools, memory and an orchestration loop"));
        assert!(!system.contains("orchestration loop"));
    }

    #[test]
    fn brief_is_context_and_guidance_appends_to_the_system_prompt() {
        let mut meeting = ctx(vec![entry(1, "them", "hello")], Vec::new(), 1);
        meeting.context = "Senior Java backend interview".into();
        meeting.instructions = "one sentence, correct my grammar".into();
        let (system, user) =
            meeting.messages("what is a record?", Budget::for_request(Mode::Interview, false));
        assert!(user.contains("What this session is about"));
        assert!(user.contains("Senior Java backend interview"));
        assert!(system.contains("one sentence, correct my grammar"));
        // Guidance must never outrank the mode's own rules.
        assert!(system.contains("Follow it where it does not conflict with the rules above"));

        // Without a brief the prompt is the Phase-3 one, byte for byte.
        let plain = ctx(vec![entry(1, "them", "hello")], Vec::new(), 1);
        let (plain_system, plain_user) =
            plain.messages("what is a record?", Budget::for_request(Mode::Interview, false));
        assert!(!plain_user.contains("What this session is about"));
        assert!(!plain_system.contains("answer guidance"));
    }

    #[test]
    fn about_me_is_background_not_transcript() {
        let mut meeting = ctx(vec![entry(1, "them", "hello")], Vec::new(), 1);
        meeting.about_me = "Five years of Spring Boot; I prefer short answers.".into();
        let (_, user) =
            meeting.messages("what is a record?", Budget::for_request(Mode::Interview, false));
        assert!(user.contains("About me, from my own settings"));
        assert!(user.contains("Five years of Spring Boot"));
        assert!(user.contains("never invent anything beyond it"));

        // Empty text changes nothing.
        let plain = ctx(vec![entry(1, "them", "hello")], Vec::new(), 1);
        let (_, user) = plain.messages("q", Budget::for_request(Mode::Interview, false));
        assert!(!user.contains("About me"));
    }

    #[test]
    fn prompt_separates_earlier_excerpts_and_forbids_invention() {
        let ctx = ctx(
            vec![entry(5, "them", "deploy on tuesday")],
            vec![entry(1, "me", "budget is fixed")],
            9,
        );
        let budget = Budget::for_request(Mode::Meeting, false);
        let (system, user) = ctx.messages("what about the budget?", budget);
        assert!(user.contains("Relevant earlier excerpts:"));
        assert!(user.contains("budget is fixed"));
        assert!(user.contains("Most recent transcript:"));
        assert!(user.contains("deploy on tuesday"));
        assert!(user.contains("Question: what about the budget?"));
        assert!(system.contains("never invent"));
    }

    #[test]
    fn interview_mode_answers_the_far_side_from_general_knowledge() {
        let ctx = ctx(vec![entry(1, "them", "What is Java?")], vec![], 1);
        let budget = Budget::for_request(Mode::Interview, false);
        let (system, user) = ctx.messages("What is Java?", budget);
        assert!(user.contains("interviewer just said: What is Java?"));
        assert!(user.contains("Answer it as the candidate"));
        assert!(system.contains("general technical knowledge"));
        assert!(system.contains("Never invent personal facts"));
        // Meeting mode must never invite outside knowledge: it is a record.
        let (meeting_system, _) = ctx.messages("What is Java?", Budget::for_request(Mode::Meeting, false));
        assert!(!meeting_system.contains("general technical knowledge"));
    }

    #[test]
    fn previous_answers_are_carried_as_assistant_memory() {
        let mut ctx = ctx(vec![entry(7, "them", "and the second one?")], vec![], 7);
        ctx.previous = vec![PreviousAnswer {
            question: "What is a HashMap?".into(),
            answer: "A hash table keyed by object hashes.".into(),
        }];
        let (_, user) = ctx.messages(
            "and the second one?",
            Budget::for_request(Mode::Interview, true),
        );
        assert!(user.contains("What you already answered in this meeting"));
        assert!(user.contains("A hash table keyed by object hashes."));
        // ...and it comes before the transcript, so the model reads it as prior state.
        assert!(
            user.find("A hash table").unwrap() < user.find("Most recent transcript").unwrap()
        );
    }

    #[test]
    fn ai_help_sessions_use_interview_mode() {
        assert_eq!(Mode::for_session(true), Mode::Interview);
        assert_eq!(Mode::for_session(false), Mode::Meeting);
    }

    #[test]
    fn local_interview_stays_short_and_cloud_sees_the_whole_meeting() {
        let local = Budget::for_request(Mode::Interview, false);
        let cloud = Budget::for_request(Mode::Interview, true);
        // A small local model has to prefill this before it answers.
        assert!(local.recent_entries < cloud.recent_entries);
        assert!(!local.searches_older_lines);
        assert!(cloud.searches_older_lines);
        assert!(cloud.max_chars > local.max_chars);
        assert!(cloud.previous_answers > local.previous_answers);
        // Meetings are not latency-critical the same way; both get the window.
        assert_eq!(Budget::for_request(Mode::Meeting, false).recent_entries, RECENT_ENTRIES);
    }

    #[test]
    fn empty_meeting_is_stated_not_implied() {
        let ctx = ctx(vec![], vec![], 0);
        let (_, user) = ctx.messages("summarize", Budget::for_request(Mode::Meeting, false));
        assert!(user.contains("no transcript yet"));
    }

    #[test]
    fn oversized_single_line_is_truncated_not_dropped() {
        let long = "x".repeat(500);
        let text = render_entries(&[entry(1, "them", &long)], 50);
        assert!(text.contains('x'), "the only line must not disappear");
        assert!(text.trim_end().chars().count() <= 50);
    }

    #[test]
    fn earlier_answers_keep_the_newest_pairs_when_over_budget() {
        let previous = vec![
            PreviousAnswer {
                question: "old ".repeat(20),
                answer: "old answer".into(),
            },
            PreviousAnswer {
                question: "newest question".into(),
                answer: "newest answer".into(),
            },
        ];
        let text = render_previous(&previous, 60);
        assert!(text.contains("newest answer"));
        assert!(!text.contains("old answer"));
    }
}
