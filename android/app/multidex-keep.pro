# System/priv-app + Magisk overlays often fail to load secondary dex at process start.
# Keep our entry points in the primary dex.
-keep class com.pixelcallai.streamer.** { *; }
