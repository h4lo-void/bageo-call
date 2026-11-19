![Bageo: your number, answered by AI. Answers calls, stalls scammers, runs errands.](https://cdn.jsdelivr.net/gh/h4lo-void/bageo-call@v1.6.9/docs/images/banner.en.svg)

**An AI phone assistant for the phone number you already have.** Gemini Live answers your calls and makes calls for you. It is self-hosted and open source: a spare rooted Android phone with your SIM, this container on a computer or NAS that stays on, and your own Gemini key.

**Ready in about 3 minutes** once the phone is rooted. One ring, and the AI answers.

[GitHub](https://github.com/h4lo-void/bageo-call) · [Releases](https://github.com/h4lo-void/bageo-call/releases) · [Getting started](https://github.com/h4lo-void/bageo-call/blob/main/docs/en/GETTING_STARTED.md) · [All features](https://github.com/h4lo-void/bageo-call/blob/main/docs/en/FEATURES.md) · [繁體中文](#繁體中文)

## What it does

- **Answers your calls.** It picks up after one ring, talks like a person and waits for the caller to say goodbye. You get a summary after every call.
- **Stalls scammers.** It spots sales and scam calls, then plays a slow, curious customer for up to 30 minutes. It never gives out real details. Honest calls are left alone: a bank checking a charge, a clinic, a courier, family.
- **Runs your errands.** Write one line, like "book a table for four tomorrow at 7 pm", and it makes the call. When the other side asks something you did not cover, it asks you on screen first. It never agrees to anything on its own.
- **Speaks nine languages.** English, Traditional and Simplified Chinese, Japanese, Korean, Spanish, French, German and Portuguese. When the caller switches language, it follows. Each SIM on a dual-SIM phone can have its own language.
- **Forwards urgent calls to you.** You choose who can be forwarded: the Whitelist, other contacts or numbers not in your contacts. When it is urgent, the AI can call you and join you into the call (trial).
- **Appointments wait for you.** A caller who wants to set a time lands on your to-do list. You accept or decline, and the AI texts or calls them back.
- **Knows who is calling.** Import contacts from vCard, Excel, PDF or a photo of a business card, and put people on a Whitelist, Blacklist or stall list.
- **32 notification channels.** A summary after every call on Telegram, Discord, Slack, LINE and more. Act on it with one tap, or send commands from the chat.
- **Your data stays yours.** Recordings, transcripts and settings are plain files in the container's volume. Backups can be automatic and encrypted. There is no account with us.

![A scammer calls and Bageo keeps them talking for 27 minutes without giving out any details.](https://cdn.jsdelivr.net/gh/h4lo-void/bageo-call@v1.6.9/docs/images/scene-stall.en.svg)

![An AI errand: Bageo books a table for four and accepts 7:30 pm when 7 pm is full.](https://cdn.jsdelivr.net/gh/h4lo-void/bageo-call@v1.6.9/docs/images/scene-errand.en.svg)

## Why Bageo

| | AI phone services on SIP trunks or virtual numbers | Bageo |
|---|---|---|
| **Number** | A number from the service, or your calls forwarded to it | The number you already have. Outgoing calls show it too. |
| **Cost** | A subscription or a per-minute rate | No platform fee. You pay Google for Gemini and your carrier for the SIM. |
| **Your data** | On the service's servers | Plain files on your own disk |
| **The catch** | Sign up and go | You need a spare rooted Android phone and a computer that stays on |

## How it works

![How Bageo works: the caller reaches your spare phone; the phone passes the call audio over Wi-Fi to Bageo, which talks to Gemini Live. Recordings stay on your disk and notifications go to your chat apps.](https://cdn.jsdelivr.net/gh/h4lo-void/bageo-call@v1.6.9/docs/images/how-it-works.en.svg)

The phone only carries the call. Everything else runs in this container.

## What you need

1. **A spare Android phone** with your SIM, rooted with Magisk or KernelSU, Android 10 or newer. Tested: Google Pixel 7, 7a, 8 and 9. Install `bageo-phone-module-<version>.zip` from [Releases](https://github.com/h4lo-void/bageo-call/releases) as a module.
2. **A Gemini API key** from [Google AI Studio](https://aistudio.google.com/apikey). Google bills per use.
3. **A computer or NAS that stays on** and runs Docker, on the same network as the phone.

## Quick start

```bash
docker run -d --name bageo-call --restart unless-stopped \
  -p 8787:8787 \
  -e CALLS_UI_TOKEN=choose-a-console-password \
  -e DEVICE_TOKEN=choose-a-phone-password \
  -e PUBLIC_URL=http://192.168.1.20:8787 \
  -e TZ=Europe/London \
  -v bageo_data:/data/calls \
  bageo/bageo-call:latest
```

1. Open `http://<this host>:8787/` and type the `CALLS_UI_TOKEN` value as the password.
2. Under **Settings**, fill in your Gemini key, your name and the language.
3. Pair the phone by IP address ([Phone setup → Wi-Fi](https://github.com/h4lo-void/bageo-call/blob/main/docs/en/PHONE.md#wi-fi)):
   1. In the console open **System** and, under **Pair a phone by IP address**, click **Get a pairing code**.
   2. In the **Bageo** app on the phone tap **Connect by IP address** and enter this host's IP, for example `192.168.1.20:8787`, and the code.
   3. Only port 8787 is needed. If you publish another host port (`-p 9000:8787`), type that port in the app. The phone and this host must be on the same network. Connecting to `ws://<this host>:8787/ws?token=<DEVICE_TOKEN>` by hand still works too.
4. Call your number from another phone: one ring, and the AI answers.

Or with Docker Compose:

```yaml
services:
  bageo:
    image: bageo/bageo-call:latest
    container_name: bageo-call
    restart: unless-stopped
    ports:
      - "8787:8787"
    environment:
      CALLS_UI_TOKEN: choose-a-console-password
      DEVICE_TOKEN: choose-a-phone-password
      PUBLIC_URL: http://192.168.1.20:8787
      TZ: Europe/London
    volumes:
      - bageo_data:/data/calls

volumes:
  bageo_data:
```

## Settings for the container

| | What it does |
|---|---|
| `CALLS_UI_TOKEN` | The console password. Required. |
| `DEVICE_TOKEN` | The password the phone uses to connect. Required. |
| `PUBLIC_URL` | The address used in notification links, like `http://192.168.1.20:8787`. |
| `TZ` | Time zone for notification times and "today", like `Asia/Taipei`. |
| `CALLS_RETENTION_DAYS` | How many days call records are kept. Default 15. |
| `/data/calls` | Volume with your settings, call records, recordings and backups. |
| `8787` | Port of the web console and of the phone connection. |

Everything else is set in the web console. Full list: [Settings reference](https://github.com/h4lo-void/bageo-call/blob/main/docs/en/SETTINGS.md).

## Tags

| Tag | Meaning |
|---|---|
| `latest` | The newest release |
| `1.6.9` | Exactly this version |

Every tag has `linux/amd64` and `linux/arm64`. The same images are on `ghcr.io/h4lo-void/bageo-call`.

## Good to know

- A container cannot see USB phones, so the phone pairs by IP address and connects over Wi-Fi.
- `http://bageo.local/` does not work inside a container. Use the host's IP address.
- On a Windows or Mac desktop, the installer from [Releases](https://github.com/h4lo-void/bageo-call/releases) is easier: one click to start, and USB works too.
- Recording calls and letting an AI talk for you is regulated differently from place to place. Read [Use it responsibly](https://github.com/h4lo-void/bageo-call#use-it-responsibly) before you start.

## License

[AGPL-3.0-only](https://github.com/h4lo-void/bageo-call/blob/main/LICENSE). A [commercial license](https://github.com/h4lo-void/bageo-call/blob/main/COMMERCIAL_LICENSE.md) is also available.

## 繁體中文

![Bageo：你的門號，由 AI 接聽。](https://cdn.jsdelivr.net/gh/h4lo-void/bageo-call@v1.6.9/docs/images/banner.zh-TW.svg)

**用你原本的門號，讓 AI 幫你接電話、替你打電話。** 由 Gemini Live 負責講話，整套自己架設、開放原始碼：一支已 root、插著你 SIM 卡的備用 Android 手機，這個容器跑在一直開著的電腦或 NAS 上，再加上你自己的 Gemini 金鑰。手機已經 root 好的話，大約 3 分鐘就能用。

- **接電話：** 響一聲就接，講話像真人，等對方說再見才掛，每通都有摘要。
- **擋詐騙：** 認出推銷和詐騙電話，裝成慢吞吞、好奇的客人陪他聊，最長 30 分鐘，不給任何真實資料。
- **代打電話：** 寫一句話，例如「明天晚上 7 點訂 4 位」，AI 就幫你打；對方問到你沒交代的事，會先在畫面上問你。
- **會講九種語言：** 對方換語言，AI 跟著換。
- **轉接到你的手機：** 誰打來可以轉由你決定；有急事時，AI 可以打給你、三方一起講（試驗中）。
- **資料留在自己手上：** 錄音、逐字稿、設定都是你磁碟上的檔案，備份可以自動、也可以加密。

快速開始：

```bash
docker run -d --name bageo-call --restart unless-stopped \
  -p 8787:8787 \
  -e CALLS_UI_TOKEN=自己設的控制台密碼 \
  -e DEVICE_TOKEN=自己設的手機連線密碼 \
  -e PUBLIC_URL=http://192.168.1.20:8787 \
  -e TZ=Asia/Taipei \
  -v bageo_data:/data/calls \
  bageo/bageo-call:latest
```

打開 `http://<這台主機>:8787/`，輸入 `CALLS_UI_TOKEN` 的值當密碼，在「設定」填好 Gemini 金鑰、名字和語言。容器看不到 USB 手機，也不能用 `bageo.local`，請用主機的 IP。在 Windows 或 Mac 桌機上，用 [Releases](https://github.com/h4lo-void/bageo-call/releases) 的安裝檔比較方便。

手機用 IP 位址配對：

1. 控制台打開「系統」頁，在「**用 IP 位址配對手機**」按「**產生配對碼**」。
2. 手機的 **Bageo** App 點「**用 IP 位址連線**」，輸入這台主機的 IP（例如 `192.168.1.20:8787`）和配對碼。
3. 只要開 8787。主機上的埠換成別的號碼也可以（`-p 9000:8787`），手機輸入的位址就填你對外開的那個埠。手機和主機要在同一個網路。原本的 `ws://<這台主機>:8787/ws?token=<DEVICE_TOKEN>` 也照樣能用。

完整說明：[中文 README](https://github.com/h4lo-void/bageo-call/blob/main/README.zh-TW.md) · [開始使用](https://github.com/h4lo-void/bageo-call/blob/main/docs/zh-TW/GETTING_STARTED.md)

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
