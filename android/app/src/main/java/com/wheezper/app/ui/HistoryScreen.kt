package com.wheezper.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.wheezper.app.MainViewModel
import com.wheezper.app.UiState

@Composable
fun HistoryScreen(state: UiState, vm: MainViewModel) {
    var query by rememberSaveable { mutableStateOf("") }

    LaunchedEffect(Unit) { vm.refresh() }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        SectionHeader("Meeting history", trailing = "${state.history.size}")

        OutlinedTextField(
            value = query,
            onValueChange = {
                query = it
                vm.setHistoryQuery(it)
            },
            placeholder = { Text("Search meetings by title") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        state.error?.let { Banner(it, isError = true, onDismiss = vm::dismissError) }

        val matches = if (query.isBlank()) {
            state.history
        } else {
            state.history.filter { it.title.contains(query, ignoreCase = true) }
        }

        if (matches.isEmpty()) {
            EmptyState(
                if (state.history.isEmpty()) {
                    "Finished meetings appear here. Open one to read its transcript or ask the assistant about it."
                } else {
                    "No meeting title matches \"$query\"."
                }
            )
        } else {
            matches.forEach { session ->
                SessionCard(session) { vm.openSession(session) }
            }
        }

        if (state.busy) {
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                CircularProgressIndicator(modifier = Modifier.height(20.dp))
            }
        }

        Text(
            "Transcript search lives inside a meeting (open one and use the search field). " +
                "The server keeps the full transcript; this phone only caches what it displays.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(12.dp))
        TextButton(onClick = vm::refresh) { Text("Refresh from server") }
    }
}
