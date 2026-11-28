# TODO — clap-validator-host (Collegato a `ROADMAP.md`)
Copyright (c) 2026 Alberto Gregorio Ferraris. All rights reserved.

- [ ] **v1.0:** Integrare "Orecchio Percettivo" (`core_dsp::psychoacoustics`: ITU-R 468, ATH, Mascheramento Bark, Sone, ITD/ILD) su Square Wave, Pink Noise e Program Material
- [ ] **v1.0:** Implementare `--match-reference` e `--auto-calibrate` con profili fisici di laboratorio (DAFx / Audio Precision)
- [ ] **v1.1:** Pubblicare binari CLI standalone per la validazione open-source CLAP

- [ ] **Audit & Fix Numerici/FFI (`clap-signal-host` & `clap-validator-host`):**
  - Null-check FFI (`clap_plugin_entry`, `plugin->init()`, `plugin->process()`, `audio_input`/`audio_output`) + commenti `// SAFETY:` sui blocchi C-ABI dei due tool host (mantenendo `#![forbid(unsafe_code)]` su `core-dsp`, `greg_gui_core` e su tutti i plugin).
  - Fix numerico `gain_db`: `20.0 * (out_rms / in_rms).max(1e-10).log10()` (clamp sull'argomento del log invece che sul risultato).
  - Rilevamento reale `nan_detected` (`f64::is_nan()` / `is_infinite()`) sull'output in `clap-validator-host`.
  - [x] Cross-correlazione FFT con `realfft` per la misura di latenza (riduzione tempi ~90%), finestra Hann + noise floor in `thd_percent_full` e compensazione `latency_samples` in `bode_from_fft`. (3/3 test unitari PASS, clippy 0 warning)
