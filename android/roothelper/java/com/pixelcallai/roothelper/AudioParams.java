// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.roothelper;

import java.lang.reflect.Method;

/**
 * Run by bageo-audio-helper as root with {@code app_process}: reads or sets audio HAL
 * parameters through {@code android.media.AudioSystem}, the call an app's
 * {@code AudioManager.setParameters} ends up in.
 *
 * <pre>
 *   get &lt;key&gt;          prints the reply, e.g. "incall_music_enabled=true"
 *   set &lt;key&gt;=&lt;value&gt;  prints the status; exit code 0 when it is 0
 * </pre>
 *
 * The helper checks keys and values before it starts this; one pair per call.
 */
public final class AudioParams {
    private AudioParams() {}

    public static void main(String[] args) {
        int code;
        try {
            code = run(args);
        } catch (Throwable t) {
            System.err.println(t);
            code = 4;
        }
        System.out.flush();
        System.exit(code);
    }

    private static int run(String[] args) throws Exception {
        if (args.length != 2) {
            System.err.println("usage: get <key> | set <key>=<value>");
            return 2;
        }
        Class<?> audioSystem = Class.forName("android.media.AudioSystem");
        if ("get".equals(args[0])) {
            Method m = audioSystem.getMethod("getParameters", String.class);
            Object reply = m.invoke(null, args[1]);
            System.out.print(reply == null ? "" : reply.toString());
            return 0;
        }
        if ("set".equals(args[0]) && args[1].indexOf('=') > 0 && args[1].indexOf(';') < 0) {
            Method m = audioSystem.getMethod("setParameters", String.class);
            Object status = m.invoke(null, args[1]);
            int s = status instanceof Integer ? (Integer) status : -1;
            System.out.print(s);
            return s == 0 ? 0 : 3;
        }
        System.err.println("usage: get <key> | set <key>=<value>");
        return 2;
    }
}
