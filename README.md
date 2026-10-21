# jdance

Кроссплатформенная pattern-based DAW на Rust: независимая clean-room
реализация с workflow, похожим на FL Studio (channel rack, step sequencer,
piano roll, playlist, mixer). Код FL Studio не используется и не
анализируется (см. [CR-002](docs/spec/amendment-02-details-and-plugin-hosting.md#13-clean-room-и-provenance)).

Лицензия: **GPL-3.0-or-later** ([LICENSE](LICENSE)).

## Статус

Ранняя разработка: Milestone 0 и headless-ядро Milestone 1.
Что сделано и что проверено — в [docs/status.md](docs/status.md).

Работает сейчас:
- channel rack со step sequencer, несколько паттернов, WAV-сэмплы
  (drag-and-drop или меню);
- piano roll: ноты с высотой, длительностью и velocity;
- VST3-инструменты как каналы: ноты из steps и piano roll с точностью до сэмпла;
- playlist: lanes, клипы паттернов с повторами, Pattern/Song режимы, loop;
- mixer: треки, mute/solo, sends pre/post, до 8 эффектов на трек
  (Parametric EQ, Tempo Delay, Reverb, компрессор с sidechain), safety
  limiter на master, компенсация задержки по всему графу;
- VST3-эффекты как insert'ы: scan в отдельном процессе с blocklist,
  окно редактора плагина и generic editor, state в проекте,
  отсутствующий плагин пропускает звук;
- воспроизведение через CPAL (WASAPI / CoreAudio / ALSA / JACK);
- audio settings, построенные на capabilities устройства;
- undo/redo, dirty state, safe save проекта;
- экспорт в WAV (song или pattern, хвост эффектов 5 с по умолчанию) из
  меню и без аудиоустройства (`jdance render`).

Ещё нет: ASIO-host,
автоматизация (см. план в [спецификации](docs/spec/)).

## Сборка

Нужен `rustup`; версия toolchain закреплена в `rust-toolchain.toml`.
Системные зависимости по ОС — в [docs/building.md](docs/building.md).

```sh
cargo build --release
./target/release/jdance assets/demo/demo.jdance
```

## Использование

```text
jdance [PROJECT] [--autoplay]           GUI
jdance render PROJECT OUT.wav [--loops N] [--sample-rate HZ] [--format pcm16|pcm24|float32]
jdance devices                          список backend'ов и устройств
```

Горячие клавиши: Space — play/stop, Ctrl/Cmd+Z — undo,
Ctrl/Cmd+Shift+Z — redo, Ctrl/Cmd+S — save, Ctrl/Cmd+O — open.

## Разработка

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask check        # граф зависимостей (ARCH-001) и SPDX-заголовки (LIC-001)
cargo deny check         # лицензии и advisories (BLD-012)
```

Эталонные WAV для golden-тестов обновляются только явно:
`JDANCE_UPDATE_GOLDEN=1 cargo test -p jdance-engine --test render`.

## Структура

| Crate | Назначение |
|---|---|
| `domain` | документ проекта, стабильные ID |
| `commands` | правки, undo/redo, dirty state |
| `dsp` | pan/gain, sampler voice, ресемплинг |
| `engine` | transport, scheduler, RT-процессор, offline render |
| `audio-api` | platform-neutral интерфейсы и capabilities |
| `audio-cpal` | адаптер CPAL |
| `persistence` | формат проекта, safe save, WAV |
| `plugin-api` | format-neutral типы хостинга плагинов (каркас) |
| `ui` | egui-интерфейс |
| `app` | wiring, CLI, точка входа |
| `rt-check` | детектор аллокаций в RT-коде (тесты) |
| `test-support` | тестовые сигналы, golden-файлы |
| `xtask` | проверки репозитория, генерация демо |

Спецификация: [docs/spec/](docs/spec/). Решения: [docs/adr/](docs/adr/).
