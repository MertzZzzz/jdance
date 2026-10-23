# Статус реализации

Статусы по разделу 0.2 Amendment 02. `PASS` только с артефактом:
тест в репозитории, лог CI или acceptance-отчёт.
Последнее обновление: 2026-10-22 (релиз 0.2.4).

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
| ADR-006 ASIO через CPAL или свой адаптер | ACCEPTED (0.2.2) | `docs/adr/006-asio-adapter.md`: свой адаптер `crates/audio-asio` |
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
| Generic editor: все параметры, текст значения от плагина, дискретные и read-only, ввод значения текстом (PLG-064) | PASS (ручная) | Xvfb + XTest: «0.5» → Gain 0.500 |
| Окно GUI плагина: нативное окно на ОС, attach, `resizeView`, IRunLoop на Linux, закрытие и повторное открытие (PLG-060..063) | PASS (тестовый плагин) | `plugin-vst3/tests/editor.rs` (Linux под Xvfb, Windows и macOS в CI); ручная проверка на Xvfb: кнопка Edit; реальные плагины с GUI NOT RUN |
| HiDPI, owner-окно, клавиатура для редакторов | NOT RUN | ADR-010, последствия |
| Защита от crash-loop: маркер загрузки, плагин после падения не грузится сам, «Load again» в слоте (PLG-025) | PASS | `plugin_that_crashed_last_run_is_held_back_until_retried` |
| Autosave (PLG-075) | NOT RUN | |
| Undo для изменений параметров плагина | NOT RUN | изменения в generic editor сейчас без undo |

## Milestone P2 — часть 1: Piano roll

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Ноты в паттерне: start, length (тики), key 0..127, velocity; до 10 000 на строку | PASS | `domain` валидация, `persistence` формат v7 |
| Команда SetNotes: сортировка, проверка, перетаскивание = один шаг undo | PASS | `set_notes_sorts_validates_and_drags_undo_as_one_step` |
| Укорочение паттерна учитывает ноты за концом (явный truncate) | PASS | `shortening_counts_and_drops_notes_beyond_the_end` |
| Sampler играет ноты с высотой (2^(n/12), линейная интерполяция; C5 бит-в-бит как раньше) | PASS | `notes_start_on_time_and_pitch_changes_speed`, golden-файл не изменился |
| Gate нот обрезается концом повторения паттерна; ноты за концом не играют | PASS | `gate_is_cut_at_the_repetition_end`, `notes_after_the_pattern_end_do_not_play` |
| Редактор: добавить (двойной клик), перетащить, изменить длину, удалить, мультивыделение, velocity-полоса, snap, zoom | PASS (ручная) | Xvfb + XTest |
| Piano roll с 10 000 нот: p95 ≤ 16.6 ms (BLD-020) | NOT RUN | рисуются только видимые ноты; замер на reference machine не проводился |
| Ввод нот с клавиатуры компьютера | NOT RUN | |

## Milestone P2 — часть 2: VST3-инструменты

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Канал-инструмент (источник `plugin`), формат проекта v8 | PASS | `domain`, `persistence` |
| Note on/off с `noteId`, velocity, sample offset; деление блока с пересчётом offset (PLG-033) | PASS | `instrument_receives_sample_accurate_notes` (тестовый синтезатор, журнал событий) |
| Gate: шаг держится один шаг, нота — свою длину, обрезка концом повторения | PASS | `notes_and_steps_turn_on_and_off_on_time` |
| Note-off всем звучащим нотам на loop wrap, Stop, seek, panic (PLG-033) | PASS | `loop_wrap_and_stop_turn_sounding_notes_off` |
| Latency инструмента в PDC (MIX-030) | PASS | `instrument_latency_is_compensated_against_other_channels` |
| RT-001 с инструментом и событиями | PASS | `instrument_rendering_does_not_allocate` |
| Экземпляр создаётся из state, переживает пересборку снапшотов, state сохраняется в проекте | PASS | `instrument_channels_get_live_instances`, общий реестр `PluginTarget` |
| Export с инструментами: отдельные offline-экземпляры; отсутствующий — тишина с предупреждением | PASS | `export_plays_instrument_channels` |
| UI: «+ Instrument», Edit (окно плагина), Params (generic editor), steps и piano roll для инструмента | PASS (ручная) | Xvfb |
| Pitch bend и CC через `IMidiMapping` (PLG-033) | NOT RUN | в piano roll ещё нет pitch bend/CC |
| Note-off velocity | NOT RUN | всегда 0 |
| Кнопка Panic в тулбаре: note-off всем инструментам, остановка голосов, воспроизведение продолжается | PASS | `panic_turns_notes_off_and_playback_continues` |

## Milestone A — автоматизация (часть 1: модель и движок)

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| ADR-014: дорожки на шкале песни (не clips), кривые hold/linear/curve, только Song-режим | ACCEPTED | `docs/adr/014-automation-model.md` |
| Модель: точки (тики, нормализованное значение, кривая, tension), цели AUT-002, формат AUT-060, проект v9 | PASS | `domain` и `persistence` тесты |
| Вычисление огибающей: каждая кривая, одна точка, совпадающие точки, hold, края (AUT-011, AUT-070 unit) | PASS | `curves_and_edges`, `tension_shapes_the_curve` |
| Команды с undo; перетаскивание = один шаг; одна дорожка на цель | PASS | `automation_lanes_follow_their_objects` |
| Удаление объекта удаляет его дорожки, undo восстанавливает обе; дорожки без цели сохраняются (AUT-050) | PASS | тот же тест |
| Громкость/pan/send: значение каждые 32 сэмпла, рампа внутри, hold ≤ 32 сэмпла (AUT-021) | PASS | `volume_follows_the_envelope_within_32_samples`, `hold_jumps_complete_within_32_samples`, `pan_and_send_level_are_automated` |
| Параметры встроенных эффектов (в т.ч. компрессор с sidechain) и bypass | PASS | `builtin_parameter_and_bypass_are_automated` |
| VST3: chase в начале блока (AUT-033), точки огибающей с точным offset, промежуточные каждые 64 (AUT-030) | PASS | `plugin_parameters_get_chase_breakpoints_and_intermediate_points`; журнал JD Test Logger в движке (AUT-031/033): точные offset, скачок Hold и переход loop парой точек, chase после seek — `vst3_automation_points_arrive_sample_accurately` |
| Независимость от размера блока (AUT-070 property) | PASS | `automated_rendering_is_block_size_independent` (мутационная проверка) |
| RT-001 при применении автоматизации | PASS | `applying_automation_does_not_allocate` |
| Возврат к значениям проекта после остановки и вне Song-режима (AUT-040) | PASS (код) | рампа к проектным значениям; отдельного теста нет |
| Golden render с автоматизацией volume, pan, send level (AUT-051, CI-003) | PASS | `golden_volume_pan_and_send_automation`; на старте значения подхватываются сразу (первый сэмпл, AUT-033) |

## Milestone A — автоматизация (часть 2: UI)

| Проверка | Статус | Чем подтверждено |
|---|---|---|
| Дорожки огибающих под дорожками плейлиста на той же шкале, zoom и прокрутке | PASS | ручная проверка под Xvfb |
| Добавление (двойной клик), перемещение (drag, привязка к шагу, Alt — без привязки), удаление (двойной клик по точке или меню) | PASS | ручная проверка; перемещение не выводит точку за соседние |
| Кривая Linear/Curve/Hold и tension из меню точки; перетаскивание tension — один шаг undo | PASS | ручная проверка; форма линии — `envelope_shapes` |
| «Automate» / «Remove automation» в контекстном меню: громкость, pan, send, параметры встроенных эффектов, bypass, параметры VST3 (только automatable) | PASS | ручная проверка (Kick volume, JD Test Logger: Gain, bypass); дорожка начинается с текущего значения — `add_automation_starts_from_the_current_value_once` |
| Правый клик по слайдеру не меняет значение | PASS | ручная проверка |
| Отметка автоматизированных регуляторов (оранжевый уголок) | PASS | ручная проверка |
| Подписи дорожек (параметр + трек/эффект), значения в единицах цели | PASS | `value_texts_use_target_units` |
| Дорожка без цели: «Target not found», сохраняется, удаляется кнопкой | PASS | ручная проверка с файлом, где цель отсутствует |
| Регуляторы показывают автоматизированное значение во время воспроизведения | NOT RUN | показывают проектное значение |
| Запись автоматизации движением регулятора | NOT RUN | вне MVP |

## Интерфейс по дизайну Graphite (U1–U5)

| Проверка | Статус | Чем подтверждено |
|---|---|---|
| Тема Graphite, шрифты IBM Plex (OFL), панель инструментов, строка состояния, переключатели окон | PASS | ручная проверка под Xvfb; `toolbar` тесты (позиция, tap tempo) |
| Channel Rack: ручки, номер трека, цвета, рисование шагов одним шагом undo, скорость колесом, клон/перемещение/удаление | PASS | `channels_move_clone_and_recolor`, ручная проверка |
| Channel Settings: громкость, pan, pitch, трек, полифония, mono; Sampler: маркеры, root, gain, loop, ADSR | PASS | `crates/engine/tests/sampler.rs`, `channel_settings_round_trip_and_are_validated` |
| Browser: избранное, разделы проекта (паттерны, история → undo), папки библиотеки, плагины, расположения | PASS | ручная проверка; избранное и папки сохраняются в настройках |
| Browser: листинг и поиск в фоновом потоке, папки первыми, скрытые пропускаются | PASS | `lists_folders_first_and_searches_audio` |
| Browser: детали файла с формой волны, предпрослушивание мимо микшера | PASS | `preview_plays_without_transport_and_is_released_off_thread` (движок); без устройства — уведомление |
| Перетаскивание WAV и инструментов из Browser в Channel Rack (новый канал или замена сэмпла), один шаг undo | PASS | ручная проверка |
| Перетаскивание в Playlist | NOT RUN | нужны аудиоклипы (U6) |
| Форматы кроме WAV | NOT RUN | показываются как неподдерживаемые |
| Piano Roll: инструменты Draw / Paint / Select / Delete, рамка (Ctrl или Select), выбор паттерна и канала, линейка, сводка выделения | PASS | ручная проверка под Xvfb |
| Piano Roll: Quantize (сетка, strength, начала/концы), Humanize (время, velocity, seed), Transpose, Scale (подсветка, «подогнать к ладу»), Note properties — один шаг undo | PASS | `crates/commands/src/notes.rs` тесты, `properties_keep_intervals_and_mixed_values`, ручная проверка |
| Piano Roll: Ctrl+A/C/X/V/D, Delete, стрелки (транспонирование, сдвиг по сетке) | PASS | ручная проверка (Ctrl+D, ↑) |
| Velocity нескольких нот: режимы «Relative» и «Same value» | PASS | ручная проверка |
| Прослушивание клавиш и рисуемых нот (живые ноты, также при остановленном транспорте) | PASS | `live_notes_play_while_stopped_until_note_off`, `live_notes_one_shot_and_gated`; звук под Xvfb не проверялся (нет устройства) |
| Slice | NOT RUN | следующий этап (по дизайну «Later») |

## Milestone 1A / 1B

| Пункт | Статус |
|---|---|
| ASIO-host (`audio-asio`) | NOT RUN — не начато |
| Audient hardware test | NOT RUN |
| JACK backend собирается | PASS (Linux, feature cpal/jack) |
| JACK smoke test, PipeWire-JACK | NOT RUN — нет JACK-сервера в окружении |

## Известные ограничения текущей реализации

- Ресемплинг сэмплов без anti-aliasing (BLD-041, ADR-012).


## Релиз 0.2.0 (спецификация `docs/spec/spec-0.2.0.md`)

Проверки на ветке `0.2.0`: `cargo test --workspace` — 300+ тестов
зелёные; clippy `-D warnings` для Linux, Windows (gnu) и macOS
(aarch64; для LAME build script пропускается через `DOCS_RS=1`, нативная
сборка — в CI); `cargo xtask check`. GUI — под Xvfb (X11, без оконного
менеджера) со скриншотами.

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Аудиоклипы, timebase, миграция v11 (§2) | PASS | `engine/tests/audio_clips.rs`, `persistence` тесты; GUI: drop из Browser, waveform |
| Двойной клик, Sample Editor (§3) | PASS (Xvfb) | маркеры IN/OUT меняют клип, Load into Sampler |
| Resize за оба края, Slice, PatternClip offset/duration/loop (§4) | PASS | `commands` тесты `*_edge*`, `slice_*`; GUI |
| Папки библиотеки, индекс, Sample Selection Dialog (§5) | PASS | `browser_fs` тесты (symlink-циклы), `config` тесты; GUI |
| Пути плагинов, прогресс/отмена скана (§6) | PASS (Xvfb) | окно Plugins, full rescan с прогрессом; сканер в отдельном процессе был с 0.1.0 |
| Запись входа (§7) | PASS (ALSA null, Xvfb) | `app/src/recording.rs` тесты: запись, overflow, восстановление, мониторинг; GUI-запись через ALSA null; реальное железо и выравнивание — NOT RUN |
| Draw/Paint/Delete/Slice (§8) | PASS (Xvfb) | Playlist и Piano Roll |
| Dock/Undock (§9) | PASS (Xvfb, X11) | Mixer в отдельном окне, Dock back; Windows/macOS, multi-monitor, HiDPI — NOT RUN |
| Project Browser (§10) | PASS (Xvfb) | Patterns/Samples/Automations, перетаскивание, удаление с подтверждением |
| Автоматизация клипами (§11) | PASS | `engine/tests/automation.rs` (golden без изменений, окна клипов, gain клипа), миграция v12; GUI: Automation Editor, запрет пересечений |
| MP3 export (§12) | PASS (Linux) | `mp3` и `render` тесты: CBR, info-фрейм, отмена без файла; CLI `--format mp3`; Windows/macOS — в CI |
| Миграция и сохранность (§13) | PASS | `persistence` тесты v10→v11→v12, резервная копия |
| Piano Roll Tools (§18) | PASS | `commands::note_tools` 44+ тестов, `scripts` тесты (Bjorklund golden, Arpeggiator = Arpeggiate, Sprinkler = Chop, песочница, отмена); GUI: Riff machine, Humanize, LFO |
| Последние настройки инструментов в user settings (§18.2) | PARTIAL | хранятся в памяти сеанса |
| Mixer по UI-VIS §8 | PASS (Xvfb) | полосы Compact/Wide/Wide+FX, инспектор, маршрут кликом |
| Windows MSI 0.2.0 | PASS | [релиз v0.2.0](https://github.com/MertzZzzz/jdance/releases/tag/v0.2.0); workflow `Release`: сборка, установка, проверка, удаление | workflow `Release` на ветке `0.2.0`: сборка, установка, проверка, удаление; артефакт `jdance-windows-msi` |

## Релиз 0.2.1

Проверки на ветке `0.2.1`: `cargo test --workspace`, clippy `-D warnings`,
`cargo xtask check`. GUI — под Xvfb со скриншотами.

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Перетаскивание сэмпла из библиотеки в Playlist | PASS (Xvfb) | два бага: позиция курсора при drag-and-drop и `take_payload` egui, выбрасывающий payload другого типа; тест `playlist::tests::dnd_release_keeps_a_payload_of_another_type` |
| Правая кнопка удаляет клипы (паттерны, аудио, автоматизация) | PASS (Xvfb) | клик, проход с зажатой кнопкой, один шаг отмены, Ctrl+Z |
| Правая кнопка удаляет ноты в Piano Roll | PASS (Xvfb) | быстрый проход не пропускает ноты: тест `widgets::tests::right_sweep_*` |
| Правая кнопка удаляет точки в Automation Editor; меню формы сегмента | PASS (Xvfb) | |
| Меню клипа: маркер ▾ и Shift + правый клик | PASS (Xvfb) | |
| Windows MSI 0.2.1 | PASS | [релиз v0.2.1](https://github.com/MertzZzzz/jdance/releases/tag/v0.2.1) |

## Релиз 0.2.2 (спецификация `docs/spec/spec-0.2.2.md`)

Проверки на ветке `0.2.2`: `cargo test --workspace` (313 тестов + 15 в
`audio-asio`), clippy `-D warnings` для Linux и для `audio-asio` под
Windows (gnu; приложение целиком — в Windows CI), `cargo xtask check`.
GUI — под Xvfb со скриншотами, звук — ALSA null.

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Аудиоканалы в Channel Rack, инвариант «у клипа есть канал» (ACH-001…019) | PASS | `commands::audio_channels` тесты (каждый путь создания клипа, повторный файл → тот же канал), `domain` валидация |
| Engine: клип звучит через канал, баланс канала (ACH-010…014) | PASS | `engine` тесты аудиоклипов; golden без изменений |
| Миграция v12 → v13 (ACH-060…064) | PASS | `persistence` тесты: канал на пару (сэмпл, трек дорожки), имена «x → Track» |
| Channel Rack: строка аудиоканала, фильтр, Ctrl+L, удаление с клипами, Make unique (ACH-020…036) | PASS (Xvfb) | drop из библиотеки → канал; повторный drop и Draw — тот же канал; Ctrl+L в выбранный трек; удаление канала вместе с клипами и undo |
| Playlist и Project Browser без маршрута дорожки (ACH-040…044) | PASS (Xvfb) | меню клипа: Channel, Make unique, Mixer track; каналы под сэмплом в Project Browser |
| Запись в аудиоканал, «To track» (REC-001…006) | PASS (ALSA null, Xvfb) | два дубля с «New track» → оба в треке 04 |
| ASIO: сессия, форматы, буферы, сброс, сторожевой таймер (ASIO-031…037, 050) | PASS (тестовый драйвер) | `audio-asio` тесты (15): 50 циклов открыть/закрыть, reset request во время записи (вход останавливается), остановка callbacks, panic в render → тишина, все форматы ASIO-033 и отказ от неподдерживаемых, зависший драйвер (ответ через 5 с). Не покрыто тестами: `kAsioBufferSizeChange` отдельно от reset и позиция дубля с задержками (код в UI) |
| ASIO: раскладка структур | PASS (тест размеров) | `abi` тесты; COM-код компилируется под Windows |
| ASIO на реальном драйвере: перечисление, панель, воспроизведение, запись (ASIO-052) | NOT RUN | нет Windows-машины со звуком; FlexASIO/ASIO4ALL не проверялись |
| Аппаратная приёмка Audient (ASIO-010, ASIO-053) | NOT RUN | нет устройства |
| Настройки ASIO: имена каналов, буфер в мс, задержки (ASIO-022/023) | PASS (код), на драйвере NOT RUN | под Linux ASIO нет: окно проверено с ALSA |
| Освобождение драйвера при потере фокуса (ASIO-025) | PASS (код), на драйвере NOT RUN | по умолчанию выключено |
| Вычитание задержки драйвера из позиции дубля (ASIO-042) | PASS (код), на драйвере NOT RUN | сообщение «driver latency N frames compensated» |
| Уведомление об ASIO SDK (ASIO-030) | PARTIAL | `ASIO-NOTICE.txt` в MSI; окна About в программе нет |
| Windows MSI 0.2.2 | PASS | сборка, установка, проверка версии в workflow `Release`; https://github.com/MertzZzzz/jdance/releases/tag/v0.2.2 |

Отступления от спецификации 0.2.2:

- ASIO-051: отдельной feature `asio` нет — backend собирается всегда на
  Windows; тесты сессии идут на всех трёх ОС.
- ASIO-023: CPAL-backend'ы не отдают имён каналов; подписи остаются
  «Input N» / «Outputs N / M», что совпадает с требуемым видом.
- ASIO-040: при активном ASIO в списке входов записи только играющий
  драйвер.

## Релиз 0.2.3 (спецификация `docs/spec/spec-0.2.3.md`)

Проверки на ветке `0.2.3`: `cargo test --workspace` (361 тест, из них 15
в `audio-asio`), clippy `-D warnings` для Linux и для `plugin-vst3` под
Windows (gnu), `cargo xtask check` (включая запрет литералов такта 4/4),
тест окна редактора плагина под Xvfb, smoke-тест приложения под Xvfb.
GUI — под Xvfb со скриншотами и вводом через XTEST.

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Размер такта: модель, функции такта и доли, все места 4/4 (SIG-001…005) | PASS | `domain::meter` тесты (все знаменатели), `xtask check` «bar lengths», `toolbar` тесты Snap и табло |
| Поле размера, Options › Time signature, undo (SIG-010/011) | PASS (Xvfb) | поле «3/4» в транспорте; `commands` тест `time_signature_is_one_undo_step_and_validated` |
| Сетки и линейки по размеру (SIG-012) | PASS (Xvfb) | Playlist 3/4: такты по 3 доли, подписи прореживаются |
| Формат 14, миграция 13 → 14 (SIG-020) | PASS | `persistence` тест `v13_projects_are_in_four_four_and_signatures_round_trip` |
| VST3 ProcessContext: размер и начало такта (SIG-030) | PASS (unit) | `plugin-api` тест `bar_start_follows_the_signature`; плагином не проверялось |
| Рост длины паттерна, undo одним шагом, предел, клипы (PAT-001…006) | PASS | `commands` тесты: рост до такта в 4/4 и 3/4, поздний рост в перетаскивании, предел, клипы с местом и без |
| Каретка: Seek без ограничения, Stop → каретка, конец песни (CUR-001…007) | PASS | `engine/tests/song.rs`: три теста каретки; Xvfb: клик по линейке, табло показывает каретку |
| Метроном: доли и акцент в 4/4, 3/4, 7/8, Song и Pattern; нет в экспорте (MET-001…012) | PASS | `engine/tests/metronome.rs`, `engine::metronome` тесты |
| Отсчёт: щелчки, старт на границе доли, отмена Stop, после остановки (MET-020…024) | PASS | `engine/tests/metronome.rs`; запись ждёт старта транспорта — `recording` тест `an_armed_take_waits_…` |
| Метроном/отсчёт в интерфейсе, Ctrl+M / Ctrl+P | PASS (Xvfb) | кнопки «Metro» и «1·2·3·4»; `keys` тест |
| Запись с каретки, выбор/создание дорожки (REC-010…012) | PASS (unit) | `playlist` тест `a_take_goes_to_the_armed_lane_then_the_first_free_one`; запись со звуком под Xvfb не проводилась (нет входа ALSA) |
| Привязка начала дубля к старту транспорта (REC-013) | PARTIAL | реализовано через взведённый дубль и обратный вызов вывода; погрешность ≤ одного буфера не измерена |
| Клип записи с формой волны, следование за курсором (REC-014…017) | PASS (unit), визуально NOT RUN | пики по 256 кадров — тест `takes_send_waveform_peaks_while_recording`; `dock` тест сохранения «Follow playhead» |
| Остановка записи пробелом, закрытие во время записи (REC-018/019) | PASS (код) | NOT RUN вживую |
| Входы треков: все каналы входа в CPAL и ASIO (INP-001) | PASS (тестовый драйвер ASIO) | `audio-asio` тест: `capture_all` с 2 каналами; CPAL — Windows/Linux CI сборка |
| Вход трека в движке до вставок, mute, моно в оба канала, offline без входа (INP-003/004/006) | PASS | `engine/tests/live_input.rs` |
| Передача входа в вывод: дрейф и опустошение (INP-005) | PASS | `recording` тест `live_input_reaches_the_output_callback_with_bounded_delay` |
| Поле Input, прослушивание, предупреждение, задержка (INP-002, MON-001…004, MON-020) | PASS (Xvfb) | полоса «In 1+2», выровненные полосы; `commands` тест входа и мастера |
| MON-010: «Use the input of …» в окне Recording | PASS (код) | |
| Пробел и глобальные клавиши во всех окнах; кнопка под фокусом не нажимается (KEY-001…003, 005) | PASS | `keys` тесты (повтор, текстовое поле, кнопка под фокусом) |
| Пробел в окне плагина (KEY-004) | PASS (Linux Xvfb), Windows — сборка | `plugin-vst3/tests/editor.rs`: пробел через XTEST доходит до хоста; Windows-хук проверен компиляцией и clippy |
| Входы и прослушивание на реальном устройстве, ASIO | NOT RUN | нет Windows-машины со звуком и интерфейса |
| Windows MSI 0.2.3 | PASS | сборка, установка, проверка версии в workflow `Release`; https://github.com/MertzZzzz/jdance/releases/tag/v0.2.3 |

Отступления от спецификации 0.2.3:

- PAT-005: растут и циклические клипы, показывавшие паттерн один раз
  (новые клипы создаются циклическими); спецификация уточнена.
- KEY-004: на macOS пробел в окне плагина не перехватывается; на X11 он
  перехватывается и из текстовых полей плагина (фокус внутри чужого окна
  не виден). Tap Tempo больше не отбивается пробелом — только Enter.
- INP-001: вход трека — каналы устройства, выбранного в Options ›
  Recording; ASIO и CPAL открывают все его входы (до 32).
- MON-004: для CPAL задержка прослушивания — оценка по двум буферам.


## Релиз 0.2.4 (спецификация `docs/spec/spec-0.2.4.md`)

Проверки на ветке `0.2.4`: `cargo test --workspace` (386 тестов на Linux),
clippy `-D warnings`, `cargo xtask check` (включая новую проверку
вложенных popup). Интерфейс — headless-тесты egui (новый тестовый
драйвер `ui::test_ui`: кадры с вводом мыши, поиск виджетов по
accessibility-меткам, заглушка `Host`) и приложение под Xvfb с вводом
через XTEST.

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Знаменатель размера кнопками (BUG-001/002) | PASS | `toolbar` тест `the_denominator_is_chosen_in_the_popup` |
| `xtask check`: combo box и меню внутри popup поля (BUG-003) | PASS | `xtask` тест `finds_a_combo_box_inside_a_toggle_popup`; «popups: ok» |
| Каретка тянется по линейке Playlist, при воспроизведении переход на отпускании, автопрокрутка (CUR-010…015) | PASS | `ruler` тесты жеста; `playlist` тест `the_caret_follows_a_drag_on_the_ruler`; Xvfb |
| Shift-выделение на долях, каретка на начало, Loop включается (SEL-001…005) | PASS | `ruler` тесты для 4/4 и 7/8, нулевой длины, жеста влево; `playlist` тест одной правки `SetLoop`; Xvfb: полоса, подсветка дорожек, подпись длины |
| Перенос выделения и его края, Esc (SEL-006, 008…010) | PASS | `ruler` тесты; `playlist` тест переноса; Xvfb |
| Двойной клик и меню линейки (SEL-007, SEL-011) | PASS | `playlist` тест двойного клика; Xvfb; меню — код |
| M/S и список входов на полосе, «—» без входа (INP-010…015) | PASS | `domain` тест `mono_and_stereo_toggle_keeping_the_channel`; `mixer` тесты M/S, правого клика и «—»; с реальным входом не проверялось |
| Каретка паттерна: Play, проходы, Stop (PCR-001…009) | PASS | `engine/tests/song.rs` `pattern_mode_plays_from_its_caret_and_stop_returns_there`; тесты приложения: сброс при смене паттерна; `piano` тест линейки |
| Piano Roll и Channel Rack в режиме Song (PCR-010…015) | PASS | `domain` тесты `pattern_position…`, `timeline_tick_of…`; тесты приложения: пересчёт каретки и playhead, линейка Piano Roll двигает каретку песни в клипе |
| Рендер выделения (EXP-001…009) | PASS | `render` тесты: фрагмент совпадает с отрезком полного рендера (с хвостом реверберации), после конца новых нот нет, Loop не действует, обрезка аудиоклипа с затуханием; тест окна экспорта |
| Windows MSI 0.2.4 | PASS | сборка, установка, проверка версии в workflow `Release`; https://github.com/MertzZzzz/jdance/releases/tag/v0.2.4 |

Отступления от спецификации 0.2.4:

- SEL-005: длина выделения во время жеста подписана как
  «такты:доли:тики» от нуля («1:2:000» — такт и две доли).
- PCR-004: каретка в Channel Rack — полоса цвета акцента над шагом в
  каждом ряду; видна и в режиме Song (пересчитанная каретка песни).
- EXP-005: инструменты-сэмплеры без огибающей (one-shot) звучат после
  конца выделения до конца сэмпла, как после note-off в песне.

## Релиз 0.2.5 (спецификация `docs/spec/spec-0.2.5.md`)

Проверки на ветке `0.2.5`: `cargo test --workspace` (433 теста на
Linux), clippy `-D warnings`, `cargo xtask check`. Интерфейс —
headless-тесты egui (`ui::test_ui`) и приложение под Xvfb с вводом через
XTEST (линейка Playlist, окно цвета). Открытые вопросы спецификации
решены вариантами по умолчанию; «Снять выделение» — Esc (вопрос 4).

| Раздел | Статус | Артефакт / комментарий |
|---|---|---|
| 1. Ctrl+D в конец дорожки | PASS | `commands` тесты `duplicate_goes_to_the_end_of_the_lane`, `duplicating_a_thousand_clips_is_fast` (1000 клипов ≈3 мс); Esc снимает выделение, подсказка без выделения |
| 2. Зум к каретке | PASS | `playlist` тесты `page_keys_and_levels_keep_the_caret_where_it_is`, `the_left_edge_keeps_the_anchor_in_place_and_stays_after_zero`; настройка привязки к мыши |
| 3. Средний клик меняет сэмпл | PASS | `commands` тесты замены и отказа; `ui` тест `middle_click_replaces_the_selected_channel_sample_only`; `engine/tests/sample_swap.rs` (звучащий голос доигрывает старый сэмпл) |
| 4. Продвинутый сэмплер | PASS | `domain` тесты ключей обработки и растяжения; `dsp` тесты растяжения, Tools и фейдов declick; `engine/tests/cut_groups.rs`; тест приложения `processing_refits_to_the_project_tempo_and_applies_tools`; круговой тест формата; корневая нота C5 по умолчанию |
| 5. MP3 | PASS | `app/tests/mp3_import.rs`: задержка по тегу LAME и без него, обрезанный файл с предупреждением, формат по содержимому |
| 6. Окно цвета | PASS | `domain::color` тесты; `color_picker` тесты круга и замка; тест живого предпросмотра одним шагом Undo; Xvfb |
| 7, 8. Имя и цвет по броску | PASS | `drop_name` и `ui` тесты броска на полосу и заголовок дорожки, мастер отказывает, повтор — не шаг Undo |
| 9. Пресеты названий | PASS | `name_presets` тесты фильтра, недавних, номера паттерна; `ui` тесты пресета одним шагом и Esc |
| 10. Поиск плагинов и избранное | PASS | `plugin-api::search` тесты ранжирования, «-слова», `*`, фильтров, избранного по идентификатору, 5000 плагинов; `ui` тесты F8: Enter грузит инструмент в новый канал и эффект в выбранный трек, Esc |
| Формат проекта 15 | PASS | миграция 14→15 без изменений; новые поля с умолчаниями |
| Windows MSI 0.2.5 | PASS | сборка, установка, проверка версии в workflow `Release`; https://github.com/MertzZzzz/jdance/releases/tag/v0.2.5 |

Отступления от спецификации 0.2.5:

- §4: Keep on disk только сохраняется; нет Variants, подвкладки Effects,
  Load regions/slice markers, правого меню волны и превью по клику;
  MOD X и MOD Y только сохраняются; Declicking по умолчанию Off.
- §5: нет прогресса и отмены загрузки длинных MP3 и порога памяти.
- §3: аудиоканалы средним кликом не меняются.
- §8: мультивыделения дорожек нет; порядок пресетов (§9) и избранного
  (§10) — кнопками и меню, без перетаскивания.
- §10: только VST3; нет группового добавления в избранное и
  перетаскивания эффекта на канал микшера; операторы AND/OR/NOT не
  поддерживаются (пробел — «и», минус — «не»).

## Релиз 0.2.6 (исправления по отзыву)

Проверки на ветке `0.2.6`: `cargo test --workspace` (446 тестов на
Linux), clippy `-D warnings`, `cargo xtask check`. Интерфейс —
headless-тесты egui и приложение под Xvfb (длинные имена, точка
моно/стерео).

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Клик по плагину в окне добавления добавляет его | PASS | `ui` тесты `filters_keep_the_window_and_a_click_loads_the_instrument`, `the_mixer_picker_puts_the_effect_into_its_track` |
| Смена фильтра VST3 / Favorites не закрывает окно | PASS | тот же тест: четыре переключения фильтров, окно открыто |
| Send to sampler для аудиоканала и аудиоклипа | PASS | `commands` тест `an_audio_channel_goes_into_a_new_sampler_in_one_step` |
| Названия не выходят за границы | PASS | `widgets::fit_text`; Xvfb: Channel Rack, Project Browser, полосы микшера |
| Моно/стерео — точка под фейдером | PASS | `ui` тест `the_strip_switches_mono_to_stereo_without_touching_monitoring`; Xvfb |
| VST3 с моно- и многовыходными шинами | PASS | `plugin-vst3` тесты `a_multi_output_instrument_that_refuses_stereo_everywhere_opens`, `a_mono_effect_takes_the_mix_and_feeds_both_sides` (тестовые классы JD Test Drums, JD Test Mono); Addictive Drums не проверялся — плагина нет в среде |
| Wet/Dry у каждого эффекта | PASS | `engine` тесты `wet_dry_mixes_the_effect_with_the_dry_signal`, `an_insert_at_zero_wet_renders_like_no_insert`; `commands` тест слияния; формат 16 с круговым тестом |
| Stop останавливает любой звук, кроме мониторинга | PASS | `engine` тесты `stop_ends_the_preview`, `stop_does_not_interrupt_monitoring`, `stop_silences_an_instrument_tail_until_the_next_note`, `a_plugin_effect_is_heard_again_after_stop_and_play` |
| Windows MSI 0.2.6 | PASS | сборка, установка, проверка версии в workflow `Release`; https://github.com/MertzZzzz/jdance/releases/tag/v0.2.6 |

## Релиз 0.2.7 (спецификация `docs/spec/spec-0.2.7.md`)

Проверки на ветке `0.2.7`: `cargo test --workspace` (459 тестов на
Linux), clippy `-D warnings`, `cargo xtask check`. Открытые вопросы
спецификации решены вариантами по умолчанию.

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| §3 Ctrl+B дублирует в плейлисте, Ctrl+D снимает выделение, подсказка | PASS | `playlist` тест `ctrl_b_duplicates_and_ctrl_d_only_deselects`; таблица `bindings` без конфликтов (`no_two_commands_share_keys_in_one_window`) |
| §1 Единая таблица привязок, подписи в меню, Windows/macOS | PASS | `ui::bindings`; тест подписей; окно Options › Keyboard shortcuts |
| §1 Вставка справа от каретки, выделение вставленного, буфер между окнами | PASS | `piano` тест `paste_puts_the_leftmost_note_on_the_caret_and_selects_the_copies` |
| §1 Инструменты на одну букву, режим «клавиатура как пианино» | PASS | `piano` тест `tool_letters_and_the_typing_keyboard` |
| §1 Ctrl+A, Ctrl+D, Ctrl+B, Delete, Ctrl+X | PASS | `piano` тесты выделения, дублирования и вырезания |
| §1 Стрелки: привязка, пиксель, полутон, октава; без выделения ничего | PASS | `piano` тест `arrows_move_by_snap_and_pixel_and_transpose` |
| §1 Backspace, Page Up/Down, Esc, фокус окна | PASS | `piano` тест `snap_zoom_close_and_the_key_focus` |
| §1 Заглушение нот (T) | PASS | `engine` тест `muted_notes_do_not_play`; формат 17 |
| §1 Ctrl+Z повтор/отмена, Ctrl+Alt+Z | PASS | код (`keys`, `JdanceApp`); headless-теста нет |
| §1 Модификаторы мыши (Ctrl-выделение, Alt, Shift-копия, фиксация осей, средняя кнопка) | PASS | код; ручная проверка не проводилась |
| §2 Клик по клипу делает источник активным, вставка, Esc, удалённый источник | PASS | `playlist` тесты `a_click_on_a_clip_makes_its_pattern_the_one_that_draw_places`, `a_removed_source_falls_back_to_the_first_pattern`, `audio_and_automation_clips_give_their_sources` |
| Windows MSI 0.2.7 | — | сборка в workflow `Release` после слияния |

Отступления от спецификации 0.2.7:

- §1: пункты P1 и P2 не сделаны, кроме Alt+стрелок и прокрутки к
  вставке; у нот нет панорамы, Mod X/Y, release, цвета, slide и
  portamento, поэтому буфер их не переносит.
- §1 п. 2: в режиме Ctrl+T играют только буквы раскладки пианино;
  остальные буквы по-прежнему не переключают инструменты.
- §2 п. 14 (призрак клипа) и п. 17 (хранение в проекте) — P1, не
  сделаны.
