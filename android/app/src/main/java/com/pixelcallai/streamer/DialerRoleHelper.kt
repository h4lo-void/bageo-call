package com.pixelcallai.streamer

import android.app.Activity
import android.app.role.RoleManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.telecom.TelecomManager
import android.util.Log

/**
 * Hard takeover: become default dialer + call-screening holder, and suspend
 * Google Dialer in-call UI so Telecom cannot show their full-screen CUI.
 *
 * Restore Google Dialer later (documented in android/README.md):
 *   pm enable com.google.android.dialer
 *   cmd role add-role-holder android.app.role.DIALER com.google.android.dialer
 *   telecom set-default-dialer com.google.android.dialer
 */
object DialerRoleHelper {
    const val REQ_DIALER_ROLE = 7101

    /** Competing UI packages/components on Pixel (safe to re-enable). */
    private val SUSPEND_PACKAGES = listOf(
        "com.google.android.dialer",
    )
    private val SUSPEND_COMPONENTS = listOf(
        "com.google.android.dialer/com.android.incallui.InCallServiceImpl",
        "com.google.android.dialer/com.android.incallui.InCallActivity",
        "com.google.android.dialer/com.android.incallui.LegacyInCallActivity",
    )

    fun isDefaultDialer(context: Context): Boolean {
        return try {
            val tm = context.getSystemService(TelecomManager::class.java)
            tm?.defaultDialerPackage == context.packageName
        } catch (_: Exception) {
            false
        }
    }

    fun ensureRoles(context: Context) {
        tryRootRoles(context)
        suspendCompetingDialerUi()
        if (isDefaultDialer(context)) {
            Log.i(TAG, "already default dialer")
            return
        }
        Log.w(TAG, "not default dialer yet; root/role may still settle")
    }

    fun requestDialerRole(activity: Activity) {
        ensureRoles(activity)
        if (isDefaultDialer(activity)) return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val rm = activity.getSystemService(RoleManager::class.java) ?: return
            if (rm.isRoleAvailable(RoleManager.ROLE_DIALER) && !rm.isRoleHeld(RoleManager.ROLE_DIALER)) {
                try {
                    activity.startActivityForResult(
                        rm.createRequestRoleIntent(RoleManager.ROLE_DIALER),
                        REQ_DIALER_ROLE,
                    )
                    return
                } catch (e: Exception) {
                    Log.w(TAG, "ROLE_DIALER request failed: ${e.message}")
                }
            }
        }
        try {
            val i = Intent(TelecomManager.ACTION_CHANGE_DEFAULT_DIALER)
                .putExtra(TelecomManager.EXTRA_CHANGE_DEFAULT_DIALER_PACKAGE_NAME, activity.packageName)
            activity.startActivity(i)
        } catch (e: Exception) {
            Log.w(TAG, "CHANGE_DEFAULT_DIALER failed: ${e.message}")
        }
    }

    /**
     * Suspend Google Dialer UI after we hold ROLE_DIALER.
     * Does NOT disable com.android.phone (telephony stack).
     */
    fun suspendCompetingDialerUi() {
        for (c in SUSPEND_COMPONENTS) {
            rootPm("pm disable $c")
        }
        for (pkg in SUSPEND_PACKAGES) {
            // disable-user keeps package restorable; prefer after we are default dialer.
            rootPm("pm disable-user --user 0 $pkg")
        }
        Log.i(TAG, "suspended competing dialer UI packages/components")
    }

    fun restoreCompetingDialerUi() {
        for (pkg in SUSPEND_PACKAGES) {
            rootPm("pm enable $pkg")
        }
        for (c in SUSPEND_COMPONENTS) {
            rootPm("pm enable $c")
        }
        rootPm("cmd role add-role-holder android.app.role.DIALER com.google.android.dialer")
        rootPm("telecom set-default-dialer com.google.android.dialer")
        Log.i(TAG, "restored Google Dialer as default (manual recovery)")
    }

    private fun tryRootRoles(context: Context) {
        val pkg = context.packageName
        val cmds = listOf(
            "cmd role add-role-holder android.app.role.DIALER $pkg",
            "cmd role add-role-holder android.app.role.CALL_SCREENING $pkg",
            "telecom set-default-dialer $pkg",
            "cmd appops set $pkg ACCESS_NOTIFICATION_POLICY allow",
        )
        for (c in cmds) {
            rootPm(c)
        }
    }

    private fun rootPm(cmd: String) {
        try {
            val p = Runtime.getRuntime().exec(arrayOf("su", "-c", cmd))
            val finished = p.waitFor(5, java.util.concurrent.TimeUnit.SECONDS)
            if (!finished) {
                p.destroyForcibly()
                Log.w(TAG, "root cmd timeout: $cmd")
                return
            }
            Log.i(TAG, "root: $cmd exit=${p.exitValue()}")
        } catch (e: Exception) {
            Log.w(TAG, "root cmd failed ($cmd): ${e.message}")
        }
    }

    private const val TAG = "DialerRoleHelper"
}
