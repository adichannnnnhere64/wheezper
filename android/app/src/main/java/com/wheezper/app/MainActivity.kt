package com.wheezper.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.lifecycle.viewmodel.compose.viewModel
import com.wheezper.app.ui.WheezperApp
import com.wheezper.app.ui.WheezperTheme

/**
 * Companion app for wheezper: watch a live meeting transcript, browse history
 * and ask the AI assistant about the meeting.
 *
 * There is deliberately no audio capture or model in this app — the laptop
 * transcribes, the server stores, the phone views.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            WheezperTheme {
                WheezperApp(vm = viewModel())
            }
        }
    }
}
