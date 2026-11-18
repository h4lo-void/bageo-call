#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

# Bageo - double-click to start (macOS). See start.sh for what it does.
cd "$(dirname "$0")" || exit 1
exec ./start.sh "$@"
