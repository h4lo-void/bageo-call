// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package de.robv.android.xposed;

import java.util.Collections;
import java.util.Set;

public final class XposedBridge {
    private XposedBridge() {}

    public static void log(String text) {}

    public static void log(Throwable t) {}

    public static Set<XC_MethodHook.Unhook> hookAllMethods(
            Class<?> hookClass, String methodName, XC_MethodHook callback) {
        return Collections.emptySet();
    }
}
