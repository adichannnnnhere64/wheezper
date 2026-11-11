package com.wheezper.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.wheezper.app.MainViewModel
import com.wheezper.app.UiState

@Composable
fun SettingsScreen(state: UiState, vm: MainViewModel) {
    var server by rememberSaveable(state.settings.serverUrl) {
        mutableStateOf(state.settings.serverUrl)
    }
    var code by rememberSaveable { mutableStateOf("") }
    var name by rememberSaveable(state.settings.deviceName) {
        mutableStateOf(state.settings.deviceName)
    }
    var aiModel by rememberSaveable(state.settings.assistantModel) {
        mutableStateOf(state.settings.assistantModel)
    }
    var aiToken by rememberSaveable(state.settings.assistantToken) {
        mutableStateOf(state.settings.assistantToken)
    }
    var aiCloud by rememberSaveable(state.settings.assistantCloud) {
        mutableStateOf(state.settings.assistantCloud)
    }
    var aiEveryLine by rememberSaveable(state.settings.assistantEveryLine) {
        mutableStateOf(state.settings.assistantEveryLine)
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        SectionHeader("Connection")

        OutlinedTextField(
            value = server,
            onValueChange = { server = it },
            label = { Text("Server address") },
            placeholder = { Text("https://meeting.example.com:50051") },
            singleLine = true,
            keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                keyboardType = KeyboardType.Uri,
                imeAction = ImeAction.Done,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { vm.saveServer(server) },
                enabled = server.isNotBlank(),
            ) {
                Text("Save address")
            }
            OutlinedButton(onClick = vm::refresh, enabled = state.settings.paired) {
                Text("Test connection")
            }
        }

        SectionHeader("Device pairing")
        Text(
            if (state.settings.paired) {
                "Paired as \"${state.settings.deviceName}\". The server issued a short-lived " +
                    "device token; no permanent credential is stored in the app."
            } else {
                "Not paired. Use the pairing code printed by wheezper-server."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            label = { Text("Device name") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = code,
            onValueChange = { code = it },
            label = { Text("Pairing code") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { vm.pair(server, code, name) },
                enabled = !state.busy && server.isNotBlank() && code.isNotBlank(),
            ) {
                Text(if (state.settings.paired) "Re-pair" else "Pair")
            }
            if (state.settings.paired) {
                OutlinedButton(onClick = vm::forgetDevice) { Text("Forget device") }
            }
        }

        state.notice?.let { Banner(it, isError = false, onDismiss = vm::dismissNotice) }
        state.error?.let { Banner(it, isError = true, onDismiss = vm::dismissError) }

        SectionHeader("AI help")
        Text(
            "Used by AI-help sessions at creation time. The assistant itself runs on the backend; " +
                "this phone chooses the model and (for Ollama Cloud) supplies the token. Answers " +
                "are suggestions and never modify the transcript.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedTextField(
            value = aiModel,
            onValueChange = { aiModel = it },
            label = { Text("Model") },
            placeholder = { Text("gemma3:1b") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = aiToken,
            onValueChange = { aiToken = it },
            label = { Text("Ollama token (required for Ollama Cloud)") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Switch(checked = aiCloud, onCheckedChange = { aiCloud = it })
            Spacer(Modifier.padding(horizontal = 6.dp))
            Text("Use Ollama Cloud (ollama.com)", style = MaterialTheme.typography.bodyMedium)
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Switch(checked = aiEveryLine, onCheckedChange = { aiEveryLine = it })
            Spacer(Modifier.padding(horizontal = 6.dp))
            Text(
                "Answer every far-side line (not only questions)",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Button(
            onClick = { vm.saveAssistantSettings(aiModel, aiToken, aiCloud, aiEveryLine) },
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Save AI settings")
        }
        Text(
            "The token is stored only on this phone and sent to your server with each AI request " +
                "over the gRPC connection (use TLS; release builds refuse cleartext). The server " +
                "uses it for that call and never stores or logs it. Choosing Ollama Cloud sends " +
                "transcript text to ollama.com; the local option keeps it on your server.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        SectionHeader("Privacy")
        Text(
            "Only committed transcript text is uploaded. Raw audio never leaves the laptop. " +
                "Markdown notes and the pending-event queue stay local to the laptop.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        SectionHeader("App")

        Spacer(Modifier.height(4.dp))
        UpdateSection()
        TextButton(onClick = vm::refresh) { Text("Reload meetings") }
    }
}
