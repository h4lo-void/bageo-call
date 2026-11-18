# Bageo bundled sounds: background ambience and voice samples

This folder holds the audio the Bageo agent ships with: background sounds that play
quietly under the AI's voice on a phone call, and pre-recorded Gemini Live voice
samples for previews and blocked-number announcements. Use this page to see where each
sound comes from and how to add or re-record your own.

| File pattern | What it is |
|---|---|
| `ambience_<kind>.wav` | [Background sounds](#background-sounds-ambience_kindwav), all CC0 |
| `voices/<language>/<Voice>_<n>.wav` | [Voice samples](#voice-samples-voiceslanguagevoice_nwav) recorded with Gemini Live |
| `ivr_*.wav`, `hold_transfer.wav` | [Older menu flows](#optional-sounds-of-the-older-menu-flows), not shipped |

## Background sounds (`ambience_<kind>.wav`)

A background sound sets the room the AI seems to be in: a quiet bed under its voice,
chosen in the web console (*Settings → How it answers → Background sound*). The agent
loads every `ambience_<kind>.wav` it finds here at start-up (16 kHz mono, 16-bit PCM).

All recordings are **CC0 (public domain)** from [Freesound](https://freesound.org).
The numbers below are Freesound sound IDs (`https://freesound.org/s/<id>/`). Sounds
marked "synthesised" were generated for this project and carry no third-party rights.

Each recording was:

1. cut to stretches without intelligible speech,
2. band-limited to telephone range (150 Hz – 7 kHz) and tamed for sudden peaks,
3. cross-faded into a seamless loop,
4. normalised to −30 dBFS RMS.

The agent plays it at about −50 / −44 / −38 dBFS for level 1 / 2 / 3; the AI's voice
sits around −16 dBFS.

| File | Length | Sources |
|---|---|---|
| `ambience_office.wav` | 238 s | 636268 (servozero, office room tone) + 862604 (BeaconStudio, keyboard / mouse / chair, re-cut) |
| `ambience_home.wav` | 186 s | 164739 (living room, birds outside) + 427860 (leonelmail, fridge / air-conditioning hum) |
| `ambience_street.wav` | 316 s | 835887 (Artem_Zaitsev, daytime street: scooters, cars, buses) |
| `ambience_hospital.wav` | 172 s | 797848 (SonnySoundOfTaiwan, hospital lobby, two stretches without nearby talk) + 733585 (misperpodcast, reception waiting area) + synthesised queue chime and bedside-monitor beeps |
| `ambience_ambulance.wav` | 148 s | first 150 s of `ambience_street.wav` + 668608 (potsi, ambulance siren) as three pass-bys |
| `ambience_typhoon.wav` | 178 s | 656506 (calebjay, autumn typhoon) |
| `ambience_storm.wav` | 238 s | 376810 (Alex_hears_things, heavy rain and thunder) |
| `ambience_construction.wav` | 188 s | 627110 (ilmari_freesound, construction site: drilling, hammering) |
| `ambience_mrt.wav` | 206 s | 415517 (Martin.Sadoux, inside a metro carriage) |
| `ambience_nightmarket.wav` | 178 s | 718917 (lastraindrop, busy night-market street) |
| `ambience_firecrackers.wav` | 148 s | 752436 (lastraindrop, night-market crowd) + 381868 (Lenguaverde, New Year firecrackers) |
| `ambience_car.wav` | 238 s | 397115 (Kinoton, driving, car interior) + synthesised turn-signal ticks |

Distant, unintelligible crowd noise remains in the night market, hospital and
construction beds.

### Adding your own

- **Replace a sound:** overwrite the file of an existing kind with a 16 kHz mono
  16-bit WAV. No code change is needed.
- **Add a new kind:** drop `ambience_<kind>.wav` here, add the kind to `KINDS` in
  `crates/agent/src/ambience.rs` and add its name to the web console's translations.

Use material you have the right to distribute, without recognisable voices.

## Voice samples (`voices/<language>/<Voice>_<n>.wav`)

For every Gemini Live voice the settings page offers, in every language, this folder
holds the three preview lines (`_0`, `_1`, `_2`) and the blocked-number
announcement (`_reject`). A preview or a blocked call therefore never spends anyone's
Gemini quota. Format: 16 kHz mono IMA ADPCM WAV, spoken by Gemini Live (model
`gemini-3.8-live`).

To record them:

    GEMINI_API_KEY=… agent make-voice-samples assets/voices

The command only records what is missing (delete a file to record it again) and checks
every recording against Gemini's transcript of it. After changing a line in
`locale.rs` (`preview`, `unavailable`), delete that language's files and run it
again.

The agent uses a sample only when it runs at the same sample rate (`SAMPLE_RATE`,
default 16000); otherwise it records its own on first use.

## Optional sounds of the older menu flows

The agent also looks here for `ivr_*.wav` and `hold_transfer.wav`: greeting,
voicemail and hold-music clips of the older menu-based modes. None are shipped. The
default mode, an AI voice answering directly, does not use them, and the agent runs
without them.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
