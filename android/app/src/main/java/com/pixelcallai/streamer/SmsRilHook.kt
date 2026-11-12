package com.pixelcallai.streamer

import android.app.Application
import android.content.Intent
import android.telephony.SmsMessage
import de.robv.android.xposed.IXposedHookLoadPackage
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge
import de.robv.android.xposed.XposedHelpers
import de.robv.android.xposed.callbacks.XC_LoadPackage
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest

class SmsRilHook : IXposedHookLoadPackage {
    override fun handleLoadPackage(lpparam: XC_LoadPackage.LoadPackageParam) {
        if (lpparam.packageName != "com.android.phone") return
        try {
            val cls = XposedHelpers.findClass(
                "com.android.internal.telephony.InboundSmsHandler",
                lpparam.classLoader,
            )
            val hook = object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    capture(param.args)
                }
            }
            val n = XposedBridge.hookAllMethods(cls, "dispatchIntent", hook).size
            XposedBridge.log("SmsRil: dispatchIntent hooks=$n")
            if (n == 0) {
                val n2 = XposedBridge.hookAllMethods(cls, "processMessagePart", hook).size
                XposedBridge.log("SmsRil: processMessagePart hooks=$n2")
            }
        } catch (t: Throwable) {
            XposedBridge.log("SmsRil: hook fail ${t.javaClass.simpleName}")
        }
    }

    private fun capture(args: Array<Any?>?) {
        if (args == null) return
        for (arg in args) {
            if (arg is Intent) {
                captureIntent(arg)
                return
            }
        }
        for (arg in args) {
            if (arg != null && arg.javaClass.name.contains("InboundSmsTracker")) {
                captureTracker(arg)
                return
            }
        }
    }

    private fun captureIntent(intent: Intent) {
        val pdus = intent.getSerializableExtra("pdus") as? Array<*> ?: return
        val format = intent.getStringExtra("format")
        decode(pdus, format)
    }

    private fun captureTracker(tracker: Any) {
        try {
            val pdus = XposedHelpers.callMethod(tracker, "getPdus") as? Array<*> ?: return
            val format = XposedHelpers.callMethod(tracker, "getFormat") as? String
            decode(pdus, format)
        } catch (t: Throwable) {
            XposedBridge.log("SmsRil: tracker fail ${t.javaClass.simpleName}")
        }
    }

    private fun decode(pdus: Array<*>, format: String?) {
        val body = StringBuilder()
        var addr: String? = null
        var ts = 0L
        val raw = StringBuilder()
        for (p in pdus) {
            val bytes = p as? ByteArray ?: continue
            raw.append(bytes.size).append(':')
            for (b in bytes) raw.append(b.toInt() and 0xff).append(',')
            val msg = try {
                if (format != null) SmsMessage.createFromPdu(bytes, format)
                else @Suppress("DEPRECATION") SmsMessage.createFromPdu(bytes)
            } catch (_: Throwable) {
                null
            } ?: continue
            if (msg.isStatusReportMessage) return
            if (addr.isNullOrBlank()) addr = msg.displayOriginatingAddress
            val part = msg.displayMessageBody ?: msg.messageBody
            if (!part.isNullOrEmpty()) body.append(part)
            if (msg.timestampMillis > ts) ts = msg.timestampMillis
        }
        if (addr.isNullOrBlank() || body.isEmpty()) return
        drop(addr, body.toString(), if (ts > 0L) ts else System.currentTimeMillis(), raw.toString())
    }

    private fun drop(addr: String, body: String, ts: Long, pduKey: String) {
        try {
            val app = currentApp() ?: return
            val dir = File(app.createDeviceProtectedStorageContext().cacheDir, "pixel-sms-push")
            if (!dir.exists() && !dir.mkdirs()) return
            val id = sha(pduKey).substring(0, 32)
            val out = File(dir, "$id.json")
            if (out.exists()) return
            val json = JSONObject()
                .put("v", 1)
                .put("addr", addr)
                .put("body", body)
                .put("ts", ts)
                .toString()
            val tmp = File(dir, "$id.json.tmp")
            tmp.writeText(json, Charsets.UTF_8)
            if (!tmp.renameTo(out)) {
                out.writeText(json, Charsets.UTF_8)
                tmp.delete()
            }
            XposedBridge.log("SmsRil: drop len=${body.length}")
        } catch (t: Throwable) {
            XposedBridge.log("SmsRil: drop fail ${t.javaClass.simpleName}")
        }
    }

    private fun currentApp(): Application? {
        return try {
            val at = XposedHelpers.findClass("android.app.ActivityThread", null)
            XposedHelpers.callStaticMethod(at, "currentApplication") as? Application
        } catch (_: Throwable) {
            null
        }
    }

    private fun sha(s: String): String {
        val dig = MessageDigest.getInstance("SHA-256").digest(s.toByteArray(Charsets.UTF_8))
        return dig.joinToString("") { "%02x".format(it) }
    }
}
