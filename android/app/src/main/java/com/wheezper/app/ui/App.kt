@file:OptIn(ExperimentalMaterial3Api::class)

package com.wheezper.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.wheezper.app.MainViewModel

private enum class Tab(val label: String) {
    Meetings("Meetings"),
    History("History"),
    Settings("Settings"),
}

@Composable
fun WheezperApp(vm: MainViewModel) {
    val state by vm.state.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableStateOf(Tab.Meetings) }

    val open = state.openSession
    BackHandler(enabled = open != null) { vm.closeSession() }

    if (open != null) {
        SessionScreen(state = state, vm = vm)
        return
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("wheezper", style = MaterialTheme.typography.titleLarge) },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
        bottomBar = {
            NavigationBar(containerColor = MaterialTheme.colorScheme.surface) {
                Tab.entries.forEach { entry ->
                    NavigationBarItem(
                        selected = tab == entry,
                        onClick = { tab = entry },
                        icon = {
                            Text(
                                when (entry) {
                                    Tab.Meetings -> "◉"
                                    Tab.History -> "≡"
                                    Tab.Settings -> "⚙"
                                },
                                maxLines = 1,
                                overflow = TextOverflow.Clip,
                            )
                        },
                        label = { Text(entry.label) },
                    )
                }
            }
        },
    ) { padding ->
        Box(
            modifier = Modifier.padding(padding),
        ) {
            when (tab) {
                Tab.Meetings -> MeetingsScreen(state, vm, onOpenHistory = { tab = Tab.History })
                Tab.History -> HistoryScreen(state, vm)
                Tab.Settings -> SettingsScreen(state, vm)
            }
        }
    }
}
