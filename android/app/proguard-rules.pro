# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

-keep class com.pixelcallai.streamer.** { *; }
-keepclassmembers class com.pixelcallai.streamer.** { *; }
-keep class kotlin.** { *; }
-keep class kotlin.Metadata { *; }
-dontwarn kotlin.**
-dontwarn org.java_websocket.**
-dontwarn de.robv.android.xposed.**
-keepattributes *Annotation*,InnerClasses,Signature,EnclosingMethod