# Статус реализации

Статусы по разделу 0.2 Amendment 02. `PASS` только с артефактом:
тест в репозитории, лог CI или acceptance-отчёт.
Последнее обновление: 2026-10-20 (VST3-эффекты в микшере).

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
| ADR-008 VST3 bindings | ACCEPTED | `docs/adr/008-vst3-bindings.md` (`vst3` 0.3.0, SDK 3.8) |
| ADR-009 модель процессов плагинов | ACCEPTED | `docs/adr/009-plugin-process-model.md` |
| ADR-010 окна редакторов плагинов | ACCEPTED | `docs/adr/010-plugin-windows.md` |
| VST3 spike | PASS (headless) | `plugin-vst3/tests/host.rs` против тестового плагина; GUI плагина NOT RUN |

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

## Milestone P1 (VST3 effects)

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Тестовый плагин: gain+bypass, latency 64, sidechain aux bus, журнал параметров, output param changes, non-automatable, без GUI, fault crash/hang (PLG-080) | PASS (частично) | `test-plugins/jdance-test-fx`; нет: смена latency на лету, медленный process, mono-only, resize GUI |
| Загрузка модуля: Linux `ModuleEntry`, Windows `InitDll` + безопасные флаги поиска DLL, macOS `bundleEntry` с `CFBundle` (PLG-030, PLG-101/102) | PASS (Linux), CI (Windows, macOS) | тесты на трёх ОС в CI |
| Factory, `PClassInfo2`, канонический class id (PLG-022, PLG-070) | PASS | `scan_lists_effect_classes`, `uid` тест |
| Lifecycle: component, отдельный controller, `IConnectionPoint`, state → controller, handler, buses, setup, activate, processing; teardown в обратном порядке | PASS | все тесты `host.rs` |
| Buses: main stereo, первый aux как sidechain, остальное выключено (PLG-031) | PASS | `sidechain_bus_receives_the_sidechain_signal`; mono-адаптация NOT RUN |
| Pre-allocated ProcessData, деление длинных блоков по max_block (PLG-041) | PASS | `long_blocks_are_split_at_max_block`, `host_side_processing_does_not_allocate` |
| Параметры UI → processor (offset 0) и output changes → UI (PLG-042) | PASS | `parameter_changes_reach_processor_and_output_changes_return` |
| State round-trip (PLG-074) | PASS | `state_round_trips` |
| Latency из `getLatencySamples` (PLG-050) | PASS | `latency_is_reported_and_exact`; `kLatencyChanged` → пересчёт PDC NOT RUN |
| Load/process/unload 1000 раз (PLG-081) | PASS | `load_process_unload_repeatedly`; проверка утечек памяти NOT RUN |
| Scanner out-of-process, таймаут, crash → причина (PLG-022) | PASS | `out_of_process_scan_and_blocklist_reasons`, `scanner_crash_and_hang_are_contained` |
| Пути поиска по ОС (PLG-020) | PASS (код) | `default_search_paths`; кеш и blocklist в настройках — часть 3 |
| Плагин как insert-эффект: экземпляр создаётся один раз и передаётся из снапшота в снапшот, освобождается вне audio thread | PASS | `engine/tests/plugins.rs` (fake processor) |
| PDC с плагинами по всему графу, включая sidechain (PLG-050, MIX-030) | PASS | `plugin_latency_is_compensated_on_parallel_paths`; ручная проверка в UI: latency 64 на треке и компенсация sidechain-трека |
| Transport для плагина: playing, tempo, позиция в сэмплах и PPQ, cycle (PLG-034) | PASS | `plugin_sees_transport_at_block_start`; переход через loop внутри блока не сообщается (NOT RUN) |
| RT-001 с плагинами (код хоста) | PASS | `rendering_with_plugins_does_not_allocate`, `host_side_processing_does_not_allocate` |
| Persistence: slot плагина, state в base64, формат v6; round-trip отсутствующего плагина без потерь (PLG-070/072/074) | PASS | `plugin_state_round_trips_unchanged`; вынос больших state в asset-файлы (PLG-071) NOT RUN |
| Отсутствующий плагин: placeholder пропускает звук, уведомление при открытии, причина в слоте (PLG-072) | PASS | тесты engine и render, ручная проверка в UI |
| Смена sample rate / нового движка: пересоздание экземпляров с текущим state (PLG-044) | PASS (код) | `PluginHost::sync`; интеграционного теста с устройством нет |
| `kLatencyChanged`/`kIoChanged`: пересоздание с текущим state и пересчёт PDC (PLG-035/051) | PASS (код) | тестовый плагин пока не меняет latency на лету — NOT RUN |
| Export с плагинами: отдельные offline-экземпляры из текущего state (PLG-045, spec Export) | PASS | `export_renders_plugins_with_compensated_latency`, `export_restores_plugin_state` |
| State захватывается перед сохранением и экспортом | PASS (код) | `capture_plugin_states` |
| Каталог: кеш по (path, size, mtime, arch), blocklist с причиной, инкрементальный scan при старте, full rescan (PLG-022..024) | PASS | `scan_plugin_prints_descriptors_or_fails_cleanly`; ручная проверка: тестовый плагин найден при старте |
| Пользовательские папки плагинов в настройках (PLG-020) | PASS (ручная) | окно Plugins |
| Добавление плагина из меню «+ Effect → VST3» | PASS (ручная) | скриншот на Xvfb |
| Generic editor: все параметры, текст значения от плагина, дискретные и read-only (PLG-064) | PASS (ручная) | ввод значения текстом NOT RUN |
| Окно GUI плагина: нативное окно на ОС, attach, `resizeView`, IRunLoop на Linux, закрытие и повторное открытие (PLG-060..063) | PASS (тестовый плагин) | `plugin-vst3/tests/editor.rs` (Linux под Xvfb, Windows и macOS в CI); ручная проверка на Xvfb: кнопка Edit; реальные плагины с GUI NOT RUN |
| HiDPI, owner-окно, клавиатура для редакторов | NOT RUN | ADR-010, последствия |
| Защита от crash-loop при открытии проекта (PLG-025), autosave (PLG-075) | NOT RUN | |
| Undo для изменений параметров плагина | NOT RUN | изменения в generic editor сейчас без undo |

## Milestone 1A / 1B

| Пункт | Статус |
|---|---|
| ASIO-host (`audio-asio`) | NOT RUN — не начато |
| Audient hardware test | NOT RUN |
| JACK backend собирается | PASS (Linux, feature cpal/jack) |
| JACK smoke test, PipeWire-JACK | NOT RUN — нет JACK-сервера в окружении |

## Известные ограничения текущей реализации

- Ресемплинг сэмплов без anti-aliasing (BLD-041, ADR-012).
- Piano roll и автоматизация не начаты; VST3-инструменты не начаты.
