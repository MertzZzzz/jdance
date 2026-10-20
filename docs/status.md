# Статус реализации

Статусы по разделу 0.2 Amendment 02. `PASS` только с артефактом:
тест в репозитории, лог CI или acceptance-отчёт.
Последнее обновление: 2026-10-20 (встроенные эффекты).

## Milestone 0

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Rust workspace, закреплённый toolchain (BLD-001/002) | PASS | `Cargo.toml`, `rust-toolchain.toml` (1.97.0, edition 2024) |
| GUI window | PASS (Linux, macOS, Windows CI) | Linux: Xvfb; macOS: CI. Windows: у раннера нет OpenGL 2 → запасной рендерер wgpu, 60 кадров (CI run #4); реальная Windows-машина NOT RUN |
| CI на трёх ОС (CI-001/002) | PASS | run #1 (2026-10-20): Linux, Windows, macOS — clippy, тесты, release, headless render зелёные |
| Лицензия GPL-3.0-or-later, SPDX (LIC-001) | PASS | `LICENSE`, `cargo xtask check` |
| Политика зависимостей, cargo deny (BLD-011/012) | PASS | локально и в CI run #1 |
| Граф зависимостей (ARCH-001) | PASS | `cargo xtask check` (проверено и на нарушении) |
| ADR-001 лицензия | ACCEPTED | `docs/adr/001-license.md` |
| ADR-003 потоки и RT-структуры | ACCEPTED (для M1) | `docs/adr/003-threading-and-rt.md` |
| ADR-005 panic strategy | ACCEPTED | `docs/adr/005-panic-strategy.md` |
| ADR-013 граф микшера, pan law | ACCEPTED | `docs/adr/013-mixer-graph.md` |
| ADR-002 UI-стек | OPEN | UI spike BLD-020 не проведён: пороги производительности и встраивание окна VST3 не проверены |
| ADR-006 ASIO через CPAL или свой адаптер | OPEN | |
| ADR-008/009/010 VST3 | OPEN | |
| VST3 spike | NOT RUN | |

## Milestone 1 (audible vertical slice)

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| WAV sampler, форматы BLD-040 | PASS | `persistence` тесты: PCM 8/16/24/32, float 32/64, mono/stereo, extensible |
| 16-step loop, sample-accurate триггеры | PASS | `engine/tests/render.rs` |
| Независимость от размера блока (RT-022) | PASS | property-тест, 1…2048 |
| Нет аллокаций в render (RT-001) | PASS | `engine/tests/rt_alloc.rs` |
| Golden render (CI-003) | PASS | `test-support/golden/four_on_floor_120bpm_22k.wav` |
| Offline render без устройства (ARCH-002) | PASS | `app/tests/cli_render.rs`, `jdance render` |
| Undo/redo, dirty state | PASS | `commands` тесты |
| Safe save, валидация, повреждённые файлы (PER-001/002/030) | PASS | `persistence` тесты (усечённые файлы — без panic) |
| Audio settings на capabilities (AUD-010/011) | PASS (Linux, ALSA null) | скриншот под Xvfb: окно строится из capabilities; кнопка Apply вручную не нажималась (NOT RUN), тот же путь `open_device` проверен при старте; реальное железо — NOT RUN |
| Сохранение настроек по backend/device (AUD-012) | PASS (Linux) | `settings.json` записан после открытия `alsa:null` при старте |
| Недоступное сохранённое устройство → уведомление, без молчаливого переключения | PASS (код), ручная проверка NOT RUN | `AudioHost::open_initial`; без устройства при старте открывается окно Audio settings (проверено скриншотом) |
| WASAPI smoke test (AUD-002) | NOT RUN | нет Windows-машины с аудио (CI только собирает и тестирует) |
| CoreAudio smoke test | NOT RUN | нет macOS-машины |
| ALSA smoke test на реальной карте | NOT RUN | проверено только на `alsa:null` |
| Hot-plug (AUD-004) | NOT RUN | |
| Фоновая загрузка сэмплов (BLD-022) | PASS (код) | `app/src/samples.rs` |
| Приоритет audio thread (RT-040) | NOT RUN | не реализовано явно |

## Milestone R (mixer routing)

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Треки, main out, sends pre/post, sidechain (MIX-001/010/011/020) | PASS | `domain` и `commands` тесты, `engine/tests/mixer.rs` |
| Проверка циклов с путём, включая sidechain (MIX-002) | PASS | `domain/src/mixer.rs` тесты |
| Меню routing только с допустимыми целями (MIX-021) | PASS (код + скриншот) | `Mixer::valid_destinations`, `valid_sidechain_sources` |
| Удаление трека и undo (MIX-022, MIX-061) | PASS | `commands` тест: точный undo/redo |
| Mute/pre-fader/sidechain независим от mute (MIX-012) | PASS | `engine/tests/mixer.rs` |
| Solo (MIX-012) | PASS | solo-in-place: источники и путь к master слышны; `domain` и `engine` тесты |
| PDC по графу (MIX-030), тесты MIX-035 (1–3, 5) | PASS | `engine/tests/mixer.rs`, property-тест 48 случайных графов; мутационная проверка: без PDC 4 теста падают |
| PDC для автоматизации (MIX-035 (4)) | NOT RUN | автоматизации ещё нет |
| Лимит компенсации (MIX-031) | PASS (код) | `MAX_PDC_SAMPLES`, флаг в UI |
| Состояние не сбрасывается при правке, без щелчков (MIX-032, MIX-011) | PASS | `volume_change_is_smoothed_and_keeps_delay_state` |
| Bypass сохраняет latency (MIX-033) | PASS | `dsp` тест компрессора |
| Latency по трекам и master в UI (MIX-034) | PASS (скриншот) | панель Mixer |
| Встроенный компрессор с sidechain (MIX-040) | PASS | `dsp/src/compressor.rs` тесты |
| Нет аллокаций в render с полным графом (RT-001) | PASS | `engine/tests/rt_alloc.rs` |
| Формат проекта v3 + миграции v1→v2→v3 (PER-001) | PASS | `persistence` тест миграции |
| Бюджет stress-проекта (MIX-060) | PASS (контейнер), reference machine NOT RUN | `engine/tests/stress.rs` (ignored): 32 трека, 8 sends, 4 sidechain, 64 канала, 48 kHz/256 — p50 6.7%, p99 14.6%, max 24.4% бюджета на Xeon 2.8 GHz (облачный контейнер) |

## Built-in effects и master chain (spec-mixer-playlist-effects)

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Signal flow шины: inserts → gain/balance → mute/solo → meter → master → limiter → output meter | PASS | `engine/src/processor.rs`, `engine/tests/effects.rs` |
| 8 insert slots, add/remove/reorder/bypass/params/undo | PASS | `commands` тесты (`reorder_inserts_and_undo`, лимит 8) |
| Именованные параметры, schema version, без runtime-буферов в project.json | PASS | `persistence` тесты, миграция v3→v4 |
| Lifecycle prepare/process/reset/latency/tail | PASS | `dsp/src/processor.rs` |
| Parameter events с frame offset | PASS (архитектура) | `process_with_events`, тест |
| Parametric EQ: unity, response, extremes, smoothing, график | PASS | `dsp/tests/effects.rs`; график — скриншот NOT RUN (параметры свёрнуты) |
| Tempo Delay: первое эхо, feedback, sync при разных BPM, synced > 2 с, ping-pong, crossfade | PASS | `dsp/tests/effects.rs` |
| Reverb: IR, bounded, decay, тишина после, без аллокаций при смене decay | PASS | RT60 1 с → измерено 66 dB/s на 44.1/48/96 kHz |
| Safety Limiter: ceiling, stereo link, release, transients, GR meter | PASS | `dsp` и `engine` тесты |
| Все эффекты: sample rates, block sizes (бит в бит), NaN/Inf, reset | PASS | `all_effects_*` |
| Bypass crossfade без щелчка, hard reset | PASS | `effect_rt` и `engine/tests/effects.rs` |
| Fade при добавлении/удалении/перестановке эффекта | PASS | мутационная проверка: без fade тест падает (скачок 0.35) |
| Stop: fade + ограниченный reset | PASS | `stop_fades_out_without_click`, `stop_clears_effect_state` |
| Realtime/offline equivalence | PASS | property-тест, полная цепочка эффектов + limiter |
| Нет аллокаций в render (RT-001) со всеми эффектами, SetParam, Stop | PASS | `engine/tests/rt_alloc.rs` |
| Export: song (без loop) или pattern × N, тот же DSP, новые экземпляры эффектов | PASS | `app/src/render.rs` тесты |
| Export tail: 5 с по умолчанию, настраиваемый (0–60 с), предупреждение при более длинном ожидаемом tail | PASS | `default_tail_is_five_seconds`, `long_effect_tail_warns`; CLI `--tail`; диалог экспорта (визуально NOT RUN) |
| Export выровнен по таймлайну (PDC latency обрезается) | PASS | `latency_is_trimmed_so_audio_starts_on_time` |
| Float32 без скрытого clamp, limiter сохраняется | PASS | `float32_export_keeps_overs` |

## Playlist (spec-mixer-playlist-effects, раздел Playlist)

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Lanes (имя, цвет, mute/solo), без маршрутизации | PASS | `domain/src/playlist.rs` |
| PatternClip: start, repeat ≥ 1, длительность = длина паттерна · repeat | PASS | `duration_and_end_follow_pattern_length` |
| Приоритеты mute/solo: clip mute → lane mute → lane solo | PASS | `admission_priorities`, `engine/tests/song.rs` |
| Наложение клипов без дедупликации | PASS | `overlapping_clips_both_play_without_deduplication` |
| Редактирование: add/remove/move/repeat/mute, multi-select, drag = один undo | PASS | `commands/src/playlist.rs` тесты, `Edit::Batch` |
| Duplicate (после конца выделения), Make unique | PASS | `commands` тесты |
| Изменение длины паттерна: явный truncate, repeats сохраняются, end сдвигается | PASS | `WouldDropSteps`, модальный диалог в UI, `pattern_length_change_keeps_repeats_and_moves_end` |
| Pattern / Song режимы, loop range с release голосов, seek без восстановления нот | PASS | `engine/tests/song.rs` |
| Arrangement end + tail → stop; пустой arrangement не останавливает transport | PASS | `transport_stops_after_arrangement_end_and_tail`, `empty_arrangement_keeps_transport_running` |
| Блочная независимость song-воспроизведения | PASS | property-тест `song_is_block_size_independent` |
| Формат проекта v5 (миграция v4→v5: дефолтные lanes) | PASS | `persistence` тесты |
| UI: canvas, zoom, drag/resize, ruler loop/seek, переименование lane/pattern | PASS (ручная проверка на Xvfb) | скриншот демо-проекта |

## Milestone 1A / 1B

| Пункт | Статус |
|---|---|
| ASIO-host (`audio-asio`) | NOT RUN — не начато |
| Audient hardware test | NOT RUN |
| JACK backend собирается | PASS (Linux, feature cpal/jack) |
| JACK smoke test, PipeWire-JACK | NOT RUN — нет JACK-сервера в окружении |

## Известные ограничения текущей реализации

- Ресемплинг сэмплов без anti-aliasing (BLD-041, ADR-012).
- Piano roll, автоматизация, плагины — не начаты.
