package com.wheezper.app

/**
 * "Did the far side just ask me something?" for interview help.
 *
 * The committed transcript has no usable punctuation: whisper writes lowercase
 * sentences and usually drops the `?`, and an interviewer often runs two
 * questions into one line ("consequences of being a non-deterministic number
 * four, explain temperature and top p how would you get a reliable structured
 * output"). Matching `?` or a leading question word therefore misses real
 * questions, and a missed question means the candidate gets no answer at all —
 * the worse failure. The heuristic is deliberately generous: a false positive
 * costs one wasted answer, a false negative costs the interview.
 *
 * Order of the checks is cheapest-first, and every rule is about *asking*, not
 * about topic, so statements ("we deploy on tuesdays", "my background is in
 * java") stay quiet.
 */
object QuestionHeuristic {
    /** Words a question or an interview prompt starts with. */
    private val STARTERS = setOf(
        "what", "what's", "why", "how", "when", "where", "who", "whom", "whose",
        "which", "can", "could", "would", "should", "will", "do", "does", "did",
        "is", "are", "was", "were", "have", "has", "had",
        "explain", "describe", "define", "tell", "give", "walk", "compare",
        "name", "list", "implement", "design", "write", "show", "suppose",
        "imagine", "consider", "difference", "talk", "run", "let",
    )

    /**
     * Phrases that make a line a request even when it starts with something
     * else — the interviewer's setup sentence comes first in ASR output.
     */
    private val CUES = listOf(
        "how would you", "how do you", "how does", "how can", "how could",
        "what is", "what are", "what would", "what do you", "what does",
        "what about", "how about",
        "why is", "why do", "why would", "why does", "when would", "which one",
        "tell me", "walk me through", "walk through", "talk me through",
        "explain", "describe", "difference between",
        "give me an example", "give an example", "for example",
        "can you", "could you", "would you", "will you",
        "do you", "did you", "have you", "are you", "is it", "are there",
        "let's say", "suppose", "imagine", "consider",
        "any experience with", "experience with", "your approach",
    )

    /** Speech fillers a question can start with ("and what about the budget"). */
    private val LEAD_INS = setOf(
        "and", "so", "but", "ok", "okay", "now", "then", "well", "right", "also",
    )

    fun looksLikeQuestion(text: String): Boolean {
        val normalized = text.trim().lowercase()
        if (normalized.isEmpty()) return false
        // Whisper does emit a '?' often enough to be worth checking first.
        if ('?' in normalized) return true

        val words = normalized.split(Regex("[^a-z0-9']+")).filter { it.isNotEmpty() }
        // "and what about the budget" is still a question: skip the filler.
        val spoken = words.dropWhile { it in LEAD_INS }.ifEmpty { words }
        if (spoken.size < 3) return false

        val first = spoken.first().trimEnd('\'')
        if (first in STARTERS || spoken.first() in STARTERS) return true

        val flat = " " + words.joinToString(" ") + " "
        if (CUES.any { flat.contains(" $it ") }) return true

        // A long turn addressed at the candidate is a prompt even when no cue
        // matched ("...so the consequences of being non-deterministic, you tell
        // me what happens next").
        return words.size >= 8 && ("you" in words || "your" in words)
    }
}
