package com.wheezper.app.data

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import com.google.android.play.core.appupdate.AppUpdateManager
import com.google.android.play.core.appupdate.AppUpdateManagerFactory
import com.google.android.play.core.appupdate.AppUpdateOptions
import com.google.android.play.core.install.model.AppUpdateType
import com.google.android.play.core.install.model.UpdateAvailability
import com.google.android.gms.tasks.Task
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Updates come from Google Play, not from a downloaded APK.
 *
 * Play re-signs the app with *its* signing key, so a build this app downloads
 * itself is signed by a different key and the installer rejects it
 * (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`). The Play In-App Update API asks Play
 * for the update instead: it is the only in-app path that works for a
 * Play-distributed app, and it keeps the user on the store's own review step.
 *
 * A debug/sideloaded build has no Play record, so Play answers
 * "not available" — [check] maps that to [Status.NotFromPlay] and the UI says
 * so instead of offering a button that cannot work.
 */
object PlayUpdater {
    sealed interface Status {
        /** Play has no newer build for this device. */
        data object UpToDate : Status

        /** A newer build exists; [startImmediate] opens Play's update screen. */
        data class Available(val versionCode: Int) : Status

        /** Not installed from Play (debug build, sideload, no Play Store). */
        data object NotFromPlay : Status
    }

    private val options: AppUpdateOptions
        get() = AppUpdateOptions.newBuilder(AppUpdateType.IMMEDIATE).build()

    private fun manager(context: Context): AppUpdateManager =
        AppUpdateManagerFactory.create(context.applicationContext)

    suspend fun check(context: Context): Status = try {
        val info = manager(context).appUpdateInfo.awaitOrNull()
        when {
            info == null -> Status.NotFromPlay
            info.updateAvailability() == UpdateAvailability.UPDATE_AVAILABLE ->
                Status.Available(info.availableVersionCode())
            // The user already accepted an update that Play is applying.
            info.updateAvailability() == UpdateAvailability.DEVELOPER_TRIGGERED_UPDATE_IN_PROGRESS ->
                Status.Available(info.availableVersionCode())
            else -> Status.UpToDate
        }
    } catch (_: Exception) {
        Status.NotFromPlay
    }

    /**
     * Open Play's full-screen update flow. Returns false when the flow could not
     * start (no activity, Play not the installer, update not allowed).
     */
    suspend fun startImmediate(context: Context, activity: Activity?): Boolean = try {
        val activity = activity ?: context.findActivity() ?: return false
        val info = manager(context).appUpdateInfo.awaitOrNull()
        when {
            info == null -> false
            !info.isUpdateTypeAllowed(options) -> false
            else -> {
                manager(context).startUpdateFlow(info, activity, options)
                true
            }
        }
    } catch (_: Exception) {
        false
    }
}

/** Compose hands out a wrapper context; Play needs the hosting Activity. */
fun Context.findActivity(): Activity? {
    var current: Context = this
    while (current is ContextWrapper) {
        if (current is Activity) return current
        current = current.baseContext
    }
    return null
}

private suspend fun <T> Task<T>.awaitOrNull(): T? = suspendCancellableCoroutine { cont ->
    addOnSuccessListener { value -> if (cont.isActive) cont.resume(value) }
    addOnFailureListener { if (cont.isActive) cont.resumeWithException(it) }
    addOnCanceledListener { cont.cancel() }
}
