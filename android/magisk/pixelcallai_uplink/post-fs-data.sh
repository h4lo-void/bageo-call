#!/system/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

MODDIR=${0%/*}
PKG=com.pixelcallai.streamer
APK="$MODDIR/system/priv-app/$PKG/base.apk"
PERM="$MODDIR/system/etc/permissions/privapp-permissions-$PKG.xml"
if [ -d "$MODDIR/system/priv-app/$PKG" ]; then
  find "$MODDIR/system/priv-app/$PKG" -type d -exec chmod 755 {} + 2>/dev/null || true
  find "$MODDIR/system/priv-app/$PKG" -type f -exec chmod 644 {} + 2>/dev/null || true
  chcon -R u:object_r:system_file:s0 "$MODDIR/system/priv-app/$PKG" 2>/dev/null || true
fi
if [ -f "$PERM" ]; then
  chmod 644 "$PERM"
  chcon u:object_r:system_file:s0 "$PERM" 2>/dev/null || true
fi
ASSETS="$MODDIR/bageo_assets"
if [ -d "$ASSETS" ]; then
  find "$ASSETS" -type d -exec chmod 755 {} + 2>/dev/null || true
  find "$ASSETS" -type f -exec chmod 644 {} + 2>/dev/null || true
  chcon -R u:object_r:system_file:s0 "$ASSETS" 2>/dev/null || true
fi
OV=/dev/pca_ov
mkdir -p "$OV/u" "$OV/w" "$OV/merged" "$OV/pu" "$OV/pw" "$OV/pmerged"
if [ -d "$MODDIR/system/priv-app/$PKG" ]; then
  mkdir -p "$OV/u/$PKG"
  cp -a "$MODDIR/system/priv-app/$PKG/." "$OV/u/$PKG/" 2>/dev/null || true
  find "$OV/u/$PKG" -type d -exec chmod 755 {} + 2>/dev/null || true
  find "$OV/u/$PKG" -type f -exec chmod 644 {} + 2>/dev/null || true
  chcon -R u:object_r:system_file:s0 "$OV/u/$PKG" 2>/dev/null || true
  mount -t overlay overlay -o lowerdir=/system/priv-app,upperdir="$OV/u",workdir="$OV/w" "$OV/merged" 2>/dev/null || true
  if [ -f "$OV/merged/$PKG/base.apk" ]; then
    mount --bind "$OV/merged" /system/priv-app 2>/dev/null || true
  fi
fi
if [ -f "$PERM" ]; then
  cp -a "$PERM" "$OV/pu/" 2>/dev/null || true
  chmod 644 "$OV/pu/privapp-permissions-$PKG.xml" 2>/dev/null || true
  chcon u:object_r:system_file:s0 "$OV/pu/privapp-permissions-$PKG.xml" 2>/dev/null || true
  mount -t overlay overlay -o lowerdir=/system/etc/permissions,upperdir="$OV/pu",workdir="$OV/pw" "$OV/pmerged" 2>/dev/null || true
  if [ -f "$OV/pmerged/privapp-permissions-$PKG.xml" ]; then
    mount --bind "$OV/pmerged" /system/etc/permissions 2>/dev/null || true
  fi
fi
