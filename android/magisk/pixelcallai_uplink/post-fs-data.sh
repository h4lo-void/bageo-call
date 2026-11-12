#!/system/bin/sh
MODDIR=${0%/*}
PKG=com.pixelcallai.streamer
APK="$MODDIR/system/priv-app/$PKG/base.apk"
PERM="$MODDIR/system/etc/permissions/privapp-permissions-$PKG.xml"
if [ -f "$APK" ]; then
  chmod 644 "$APK"
  chcon u:object_r:system_file:s0 "$APK" 2>/dev/null || true
fi
if [ -f "$PERM" ]; then
  chmod 644 "$PERM"
  chcon u:object_r:system_file:s0 "$PERM" 2>/dev/null || true
fi
OV=/dev/pca_ov
mkdir -p "$OV/u" "$OV/w" "$OV/merged" "$OV/pu" "$OV/pw" "$OV/pmerged"
if [ -d "$MODDIR/system/priv-app/$PKG" ]; then
  mkdir -p "$OV/u/$PKG"
  cp -a "$MODDIR/system/priv-app/$PKG/." "$OV/u/$PKG/" 2>/dev/null || true
  chmod 644 "$OV/u/$PKG/base.apk" 2>/dev/null || true
  chcon u:object_r:system_file:s0 "$OV/u/$PKG" "$OV/u/$PKG/base.apk" 2>/dev/null || true
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
