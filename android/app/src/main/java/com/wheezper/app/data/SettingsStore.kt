package com.wheezper.app.data

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "wheezper")

/** Everything the app needs to reach the backend. Kept on-device only. */
data class Settings(
    val serverUrl: String = "",
    val deviceToken: String = "",
    val deviceName: String = "android",
    /** AI help defaults; the token lives only on this phone and travels per request. */
    val assistantModel: String = "gemma3:1b",
    val assistantToken: String = "",
    val assistantCloud: Boolean = false,
    val assistantEveryLine: Boolean = false,
    /** Reasoning budget sent to Ollama: off / low / medium / high. */
    val assistantThinking: String = "off",
) {
    val paired: Boolean get() = serverUrl.isNotBlank() && deviceToken.isNotBlank()
}

class SettingsStore(private val context: Context) {
    private object Keys {
        val server = stringPreferencesKey("server_url")
        val token = stringPreferencesKey("device_token")
        val name = stringPreferencesKey("device_name")
        val assistantModel = stringPreferencesKey("assistant_model")
        val assistantToken = stringPreferencesKey("assistant_token")
        val assistantCloud = booleanPreferencesKey("assistant_cloud")
        val assistantEveryLine = booleanPreferencesKey("assistant_every_line")
        val assistantThinking = stringPreferencesKey("assistant_thinking")
    }

    val flow: Flow<Settings> = context.dataStore.data.map { prefs ->
        Settings(
            serverUrl = prefs[Keys.server].orEmpty(),
            deviceToken = prefs[Keys.token].orEmpty(),
            deviceName = prefs[Keys.name].orEmpty().ifBlank { "android" },
            assistantModel = prefs[Keys.assistantModel].orEmpty().ifBlank { "gemma3:1b" },
            assistantToken = prefs[Keys.assistantToken].orEmpty(),
            assistantCloud = prefs[Keys.assistantCloud] ?: false,
            assistantEveryLine = prefs[Keys.assistantEveryLine] ?: false,
            assistantThinking = prefs[Keys.assistantThinking].orEmpty().ifBlank { "off" },
        )
    }

    suspend fun saveAssistant(
        model: String,
        token: String,
        cloud: Boolean,
        everyLine: Boolean,
    ) {
        context.dataStore.edit {
            it[Keys.assistantModel] = model.trim().ifBlank { "gemma3:1b" }
            it[Keys.assistantToken] = token.trim()
            it[Keys.assistantCloud] = cloud
            it[Keys.assistantEveryLine] = everyLine
        }
    }

    suspend fun saveThinking(level: String) {
        context.dataStore.edit { it[Keys.assistantThinking] = level.trim().ifBlank { "off" } }
    }

    suspend fun saveServer(serverUrl: String) {
        context.dataStore.edit { it[Keys.server] = serverUrl.trim() }
    }

    suspend fun saveDevice(serverUrl: String, token: String, name: String) {
        context.dataStore.edit {
            it[Keys.server] = serverUrl.trim()
            it[Keys.token] = token
            it[Keys.name] = name.trim().ifBlank { "android" }
        }
    }

    suspend fun forgetDevice() {
        context.dataStore.edit { it.remove(Keys.token) }
    }

    /** So the "start wheezper on the laptop" command can be shown again. */
    suspend fun saveSessionToken(sessionId: String, token: String) {
        context.dataStore.edit { it[stringPreferencesKey("session_token_$sessionId")] = token }
    }

    suspend fun sessionToken(sessionId: String): String? =
        context.dataStore.data.map { it[stringPreferencesKey("session_token_$sessionId")] }.first()
}
