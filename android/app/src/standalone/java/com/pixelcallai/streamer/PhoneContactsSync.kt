// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.provider.ContactsContract
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject

/**
 * Reads Android Contacts and merges them into local Bageo contacts via `/calls/api/contacts/batch`,
 * handed to the agent inside the app ([EmbeddedAgent.http]). [SyncResult.error] is a short code
 * the console translates (`no_permission`, `agent_not_ready`, `sync_failed`).
 */
object PhoneContactsSync {
    data class ContactEntry(
        val name: String,
        val phones: List<String>,
        val starred: Boolean,
    )

    data class SyncResult(
        val ok: Boolean,
        val added: Int = 0,
        val merged: Int = 0,
        val skipped: Int = 0,
        val error: String = "",
    )

    fun hasPermission(ctx: Context): Boolean =
        ContextCompat.checkSelfPermission(ctx, Manifest.permission.READ_CONTACTS) ==
            PackageManager.PERMISSION_GRANTED

    /** Pure JSON builder shared by runtime sync and JVM unit tests. */
    internal fun buildBatchPayloadJson(entries: List<ContactEntry>, starredToWhitelist: Boolean): String {
        val arr = JSONArray()
        for (e in entries) {
            if (e.phones.isEmpty()) continue
            val obj = JSONObject()
                .put("name", e.name.trim())
                .put("kind", if (starredToWhitelist && e.starred) "whitelist" else "contact")
                .put("phones", JSONArray(e.phones))
            arr.put(obj)
        }
        return JSONObject()
            .put("contacts", arr)
            .put("duplicates", "merge")
            .toString()
    }

    fun syncNow(ctx: Context, starredToWhitelist: Boolean): SyncResult {
        if (!hasPermission(ctx)) {
            return SyncResult(ok = false, error = "no_permission")
        }
        val entries = readDeviceContacts(ctx)
        if (entries.isEmpty()) {
            return SyncResult(ok = true, added = 0, merged = 0, skipped = 0)
        }
        val status = EmbeddedAgent.status()
        if (!status.ready) {
            return SyncResult(ok = false, error = "agent_not_ready")
        }
        val payload = buildBatchPayloadJson(entries, starredToWhitelist)
        return try {
            val res = EmbeddedAgent.http(
                "POST",
                "/calls/api/contacts/batch",
                mapOf("Content-Type" to "application/json; charset=utf-8"),
                payload.toByteArray(Charsets.UTF_8),
            ) ?: return SyncResult(ok = false, error = "agent_not_ready")
            if (res.status == 503) return SyncResult(ok = false, error = "agent_not_ready")
            if (!res.ok) return SyncResult(ok = false, error = "sync_failed")
            val obj = JSONObject(String(res.body, Charsets.UTF_8))
            SyncResult(
                ok = obj.optBoolean("ok", true),
                added = obj.optInt("added", 0),
                merged = obj.optInt("merged", 0),
                skipped = obj.optInt("skipped", 0),
            )
        } catch (e: Exception) {
            SyncResult(ok = false, error = "sync_failed")
        }
    }

    private fun readDeviceContacts(ctx: Context): List<ContactEntry> {
        val grouped = linkedMapOf<Long, MutableEntry>()
        val projection = arrayOf(
            ContactsContract.CommonDataKinds.Phone.CONTACT_ID,
            ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME_PRIMARY,
            ContactsContract.CommonDataKinds.Phone.NUMBER,
            ContactsContract.CommonDataKinds.Phone.STARRED,
        )
        ctx.contentResolver.query(
            ContactsContract.CommonDataKinds.Phone.CONTENT_URI,
            projection,
            null,
            null,
            "${ContactsContract.CommonDataKinds.Phone.CONTACT_ID} ASC",
        )?.use { cur ->
            val idxId = cur.getColumnIndex(ContactsContract.CommonDataKinds.Phone.CONTACT_ID)
            val idxName = cur.getColumnIndex(ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME_PRIMARY)
            val idxNum = cur.getColumnIndex(ContactsContract.CommonDataKinds.Phone.NUMBER)
            val idxStar = cur.getColumnIndex(ContactsContract.CommonDataKinds.Phone.STARRED)
            while (cur.moveToNext()) {
                val id = if (idxId >= 0) cur.getLong(idxId) else continue
                val num = if (idxNum >= 0) cur.getString(idxNum).orEmpty().trim() else ""
                if (num.isBlank()) continue
                val name = if (idxName >= 0) cur.getString(idxName).orEmpty().trim() else ""
                val starred = idxStar >= 0 && cur.getInt(idxStar) == 1
                val entry = grouped.getOrPut(id) { MutableEntry(name = name, starred = starred) }
                if (entry.name.isBlank() && name.isNotBlank()) entry.name = name
                if (starred) entry.starred = true
                if (num !in entry.phones) entry.phones += num
            }
        }
        return grouped.values.take(1500).map { ContactEntry(it.name, it.phones, it.starred) }
    }

    private class MutableEntry(
        var name: String,
        var starred: Boolean,
        val phones: MutableList<String> = mutableListOf(),
    )
}
