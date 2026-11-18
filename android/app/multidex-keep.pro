# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# System/priv-app + Magisk overlays often fail to load secondary dex at process start.
# Keep our entry points in the primary dex.
-keep class com.pixelcallai.streamer.** { *; }
