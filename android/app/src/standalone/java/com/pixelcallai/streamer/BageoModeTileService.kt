// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.graphics.drawable.Icon
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/** One-tap Quick Settings tile to toggle between AI Answering and Normal Phone mode. */
class BageoModeTileService : TileService() {
    private val listener = PhoneModeManager.Listener { refreshTile() }

    override fun onStartListening() {
        super.onStartListening()
        PhoneModeManager.addListener(listener)
        refreshTile()
    }

    override fun onStopListening() {
        PhoneModeManager.removeListener(listener)
        super.onStopListening()
    }

    override fun onClick() {
        super.onClick()
        val isAi = Prefs.isAiCallEnabled(this)
        val target = if (isAi) Prefs.PHONE_MODE_NORMAL else Prefs.PHONE_MODE_AI
        PhoneModeManager.requestSwitch(
            this,
            target,
            callBusy = !PhoneModeManager.isPhoneIdle(this),
        ) {
            refreshTile()
        }
        refreshTile(switching = true)
    }

    private fun refreshTile(switching: Boolean = false) {
        val tile = qsTile ?: return
        val isAi = Prefs.isAiCallEnabled(this)
        tile.label = getString(if (isAi) R.string.sa_tile_ai_on else R.string.sa_tile_normal)
        tile.state = when {
            switching -> Tile.STATE_UNAVAILABLE
            isAi -> Tile.STATE_ACTIVE
            else -> Tile.STATE_INACTIVE
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            tile.subtitle = getString(
                if (isAi) R.string.sa_tile_sub_ai else R.string.sa_tile_sub_normal,
            )
        }
        tile.icon = Icon.createWithResource(this, android.R.drawable.stat_sys_phone_call)
        tile.updateTile()
    }
}
