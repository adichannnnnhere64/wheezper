package com.wheezper.app

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The question heuristic decides whether interview help answers a far-side
 * line. These cases are the shapes the ASR actually produces: lowercase, no
 * punctuation, an interviewer's setup sentence before the actual question.
 */
class QuestionHeuristicTest {
    @Test
    fun answersARealInterviewerQuestionWithoutPunctuation() {
        // The reported miss: no '?', the line does not start with a question
        // word, and two questions are run together.
        assertTrue(
            QuestionHeuristic.looksLikeQuestion(
                "consequences of being a non-deterministic number four, explain temperature " +
                    "and top p how would you get a reliable structured output from a " +
                    "non-deterministic model"
            )
        )
    }

    @Test
    fun recognisesQuestionsAndInterviewPrompts() {
        listOf(
            "what is a record in java",
            "why would you use an interface instead of an abstract class",
            "can you explain the difference between jdk and jre",
            "walk me through how a hashmap resolves collisions",
            "tell me about a time you fixed a production incident",
            "describe the java memory model",
            "suppose the database is read heavy, what would you change",
            "which one would you pick for this workload",
            "how does garbage collection work in the g1 collector",
            "are there any trade offs with that approach",
            "difference between checked and unchecked exceptions",
            "what's the difference between a process and a thread",
            "you have a slow query, what do you look at",
        ).forEach { line ->
            assertTrue("should be treated as a question: $line", QuestionHeuristic.looksLikeQuestion(line))
        }
    }

    @Test
    fun leavesStatementsAlone() {
        listOf(
            "we deploy the new service next week",
            "my background is in java and spring",
            "the team is moving to kubernetes",
            "ok",
            "sure",
            "right",
            "so as i was saying yesterday",
            "we have five minutes left",
        ).forEach { line ->
            assertFalse("should not be answered: $line", QuestionHeuristic.looksLikeQuestion(line))
        }
    }

    @Test
    fun punctuationStillCountsButIsNotRequired() {
        assertTrue(QuestionHeuristic.looksLikeQuestion("deployment on tuesday?"))
        assertTrue(QuestionHeuristic.looksLikeQuestion("And what about the budget"))
    }
}
