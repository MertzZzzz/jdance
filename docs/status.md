# Статус реализации

Статусы по разделу 0.2 Amendment 02. `PASS` только с артефактом:
тест в репозитории, лог CI или acceptance-отчёт.
Последнее обновление: 2026-10-20.

## Milestone 0

| Пункт | Статус | Артефакт / комментарий |
|---|---|---|
| Rust workspace, закреплённый toolchain (BLD-001/002) | PASS | `Cargo.toml`, `rust-toolchain.toml` (1.97.0, edition 2024) |
| GUI window | PASS (Linux) | `--smoke-test-frames` под Xvfb, exit 0; воспроизведение в GUI с `alsa:null` (`--autoplay`, скриншот); Windows/macOS — NOT RUN |
| CI на трёх ОС (CI-001/002) | NOT RUN | `.github/workflows/ci.yml` написан, ещё не запускался |
| Лицензия GPL-3.0-or-later, SPDX (LIC-001) | PASS | `LICENSE`, `cargo xtask check` |
| Политика зависимостей, cargo deny (BLD-011/012) | PASS (локально) | `cargo deny check`: licenses, bans, sources, advisories ok; в CI — NOT RUN |
| Граф зависимостей (ARCH-001) | PASS | `cargo xtask check` (проверено и на нарушении) |
| ADR-001 лицензия | ACCEPTED | `docs/adr/001-license.md` |
| ADR-003 потоки и RT-структуры | ACCEPTED (для M1) | `docs/adr/003-threading-and-rt.md` |
| ADR-005 panic strategy | ACCEPTED | `docs/adr/005-panic-strategy.md` |
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
| WASAPI smoke test (AUD-002) | NOT RUN | нет Windows-машины |
| CoreAudio smoke test | NOT RUN | нет macOS-машины |
| ALSA smoke test на реальной карте | NOT RUN | проверено только на `alsa:null` |
| Hot-plug (AUD-004) | NOT RUN | |
| Фоновая загрузка сэмплов (BLD-022) | PASS (код) | `app/src/samples.rs` |
| Приоритет audio thread (RT-040) | NOT RUN | не реализовано явно |

## Milestone 1A / 1B

| Пункт | Статус |
|---|---|
| ASIO-host (`audio-asio`) | NOT RUN — не начато |
| Audient hardware test | NOT RUN |
| JACK backend собирается | PASS (Linux, feature cpal/jack) |
| JACK smoke test, PipeWire-JACK | NOT RUN — нет JACK-сервера в окружении |

## Известные ограничения текущей реализации

- Ресемплинг сэмплов без anti-aliasing (BLD-041, ADR-012).
- Изменение громкости/mute применяется скачком при замене snapshot
  (без сглаживания) — возможен щелчок.
- Один паттерн; piano roll, playlist, mixer tracks, routing,
  автоматизация, плагины — не начаты.
- В UI нет переименования каналов (команда есть в `commands`).
