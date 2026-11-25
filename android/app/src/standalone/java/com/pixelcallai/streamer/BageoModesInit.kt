// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.util.Log

/**
 * Runs once when the app's process starts (it serves no data): makes Bageo's own mode
 * once Bageo has Do Not Disturb access, and keeps it in step with every mode switch
 * ([DedicatedDndManager.onModeApplied]). Standalone only.
 */
class BageoModesInit : ContentProvider() {
    override fun onCreate(): Boolean {
        val ctx = context?.applicationContext ?: return true
        PhoneModeManager.addListener { mode -> DedicatedDndManager.onModeApplied(ctx, mode) }
        Thread({
            try {
                DedicatedDndManager.ensureRule(ctx, explicit = false)
                DedicatedDndManager.onModeApplied(ctx, PhoneModeManager.reconcileLocalMirror(ctx))
            } catch (t: Throwable) {
                Log.w("BageoModesInit", "Bageo mode set-up failed: ${t.message}")
            }
        }, "bageo-mode-init").start()
        return true
    }

    override fun query(
        uri: Uri,
        projection: Array<out String>?,
        selection: String?,
        selectionArgs: Array<out String>?,
        sortOrder: String?,
    ): Cursor? = null

    override fun getType(uri: Uri): String? = null

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0

    override fun update(
        uri: Uri,
        values: ContentValues?,
        selection: String?,
        selectionArgs: Array<out String>?,
    ): Int = 0
}
