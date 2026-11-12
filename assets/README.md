# Bundled sounds

## Background sounds (`ambience_<kind>.wav`)

The room the AI seems to be in: a quiet bed under its voice, chosen in the web
console (*Settings → How it answers → Background sound*). The agent loads every
`ambience_<kind>.wav` it finds here at start-up (16 kHz mono, 16-bit PCM).

All recordings are **CC0 (public domain)** from [Freesound](https://freesound.org);
the number is the Freesound sound ID (`https://freesound.org/s/<id>/`). Each was
cut to stretches without intelligible speech, band-limited to telephone range
(150 Hz – 7 kHz), tamed for sudden peaks, cross-faded into a seamless loop and
normalised to −30 dBFS RMS (the agent plays it at about −50 / −44 / −38 dBFS for
level 1 / 2 / 3; the AI's voice sits around −16 dBFS). Sounds marked
"synthesised" were generated for this project and carry no third-party rights.

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

Drop a 16 kHz mono 16-bit WAV named `ambience_<kind>.wav` here. A new *kind*
also has to be added to `KINDS` in `crates/agent/src/ambience.rs` and to the
names in the web console's translations; replacing the file of an existing kind
needs no code change. Use material you have the right to distribute, without
recognisable voices.

## Optional sounds of the older menu flows

The agent also looks here for `ivr_*.wav` and `hold_transfer.wav` (greeting,
voicemail and hold-music clips of the older menu-based modes). None are shipped:
the default mode — an AI voice answering directly — does not use them, and the
agent runs without them.
