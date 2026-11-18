// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.util.Log

/**
 * DISABLED for SILENT_LOCAL: never play PCM on speaker/earpiece/media.
 * TTS must go through [UplinkInjector] into the cellular uplink only.
 */
@Deprecated("Use UplinkInjector; local playback is forbidden in SILENT_LOCAL")
class TtsPlayer(private val sampleRate: Int) {
    init {
        Log.w("TtsPlayer", "local AudioTrack path disabled (sampleRate=$sampleRate)")
    }

    fun play(pcm: ByteArray) {
        // Intentionally no-op — do not leak AI audio into the room.
    }

    fun release() {}
}
