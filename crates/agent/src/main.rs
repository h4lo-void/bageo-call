// Copyright (C) 2026 h4lo-void (https://github.com/h4lo-void/bageo-call)
// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bageo_agent::cli_main().await
}
