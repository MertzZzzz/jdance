# Зависимости (BLD-010)

Прямые зависимости первого уровня. Версии закреплены в `Cargo.lock`.
Лицензии всех транзитивных зависимостей проверяет `cargo deny`
(`deny.toml`), допустимы только GPLv3-совместимые (BLD-011).

| Crate | Версия | Лицензия | Где | Назначение | Native-компоненты |
|---|---|---|---|---|---|
| cpal | 0.18.2 | Apache-2.0 | audio-cpal | audio I/O: WASAPI, CoreAudio, ALSA, JACK | Linux: libasound (ALSA), libjack (JACK) |
| eframe | 0.37.0 | MIT OR Apache-2.0 | ui, app | окно, event loop; рендереры glow (OpenGL) и wgpu (запасной: DX12/Vulkan/Metal) | Linux: libxkbcommon-x11, libGL (dlopen) |
| egui | 0.37.0 | MIT OR Apache-2.0 | ui | immediate-mode UI | — |
| rfd | 0.17.2 | MIT | ui | системные файловые диалоги | Linux: XDG desktop portal (D-Bus) |
| hound | 3.5.1 | Apache-2.0 | persistence, test-support | чтение/запись WAV | — |
| nanomp3 (+ nanomp3-core, wide) | 0.2.0 | MIT OR Apache-2.0 (текст лицензий проверен 2026-10-22) | persistence | декодирование MP3 при импорте (спецификация 0.2.5 §5): порт minimp3 на Rust без `unsafe`, пропуск ID3/APE, обрезка задержек по тегу LAME | — |
| rtrb | 0.4.0 | MIT OR Apache-2.0 | engine | wait-free SPSC очереди UI ↔ audio | — |
| serde | 1.0.229 | MIT OR Apache-2.0 | domain и др. | сериализация | — |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | persistence, app, xtask | формат проекта и настроек | — |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | несколько | типы ошибок | — |
| proptest | 1.12.0 | MIT OR Apache-2.0 | engine (dev) | property-тесты | — |
| vst3 | 0.3.0 | MIT OR Apache-2.0 | plugin-vst3, test-fx | bindings VST 3 SDK 3.8 (ADR-008) | — |
| libloading | 0.8.9 | ISC | plugin-vst3 | загрузка модулей плагинов (`dlopen`/`LoadLibraryExW`) | — |
| x11rb | 0.13.2 | MIT OR Apache-2.0 | plugin-vst3 (Linux) | окна редакторов плагинов на X11 (ADR-010) | X-сервер (XWayland под Wayland) |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | plugin-vst3, app, audio-asio (Windows) | окна редакторов плагинов (Win32); консоль для CLI-команд GUI-приложения; ASIO: COM (`CoCreateInstance`, STA), реестр `HKLM\SOFTWARE\ASIO`, цикл сообщений рабочего потока (ADR-006) | ASIO-драйвер, установленный пользователем |
| objc2, objc2-app-kit, objc2-foundation | 0.6.5 / 0.3.2 | MIT | plugin-vst3 (macOS) | окна редакторов плагинов (NSWindow) | AppKit |
| libc | 0.2.186 | MIT OR Apache-2.0 | plugin-vst3 (Linux) | `poll` для IRunLoop | — |
| mp3lame-encoder (+ mp3lame-sys 0.1.11) | 0.2.5 | LGPL-3.0 (crates), LAME 3.100: LGPL-2.0-or-later | mp3 | MP3-кодирование для экспорта (спецификация 0.2.0 §12, план D9) | LAME собирается из исходников в crate: autotools (Linux, macOS), `cc` (Windows MSVC); линковка статическая |
| rhai | 1.27.0 | MIT OR Apache-2.0 | scripts | встроенный runtime скриптов Piano Roll (спецификация 0.2.0 §18.22) | — |
| midir (+ alsa, coremidi, windows-sys) | 0.11.1 | MIT | app | MIDI-вход (спецификация 0.3.0 §12): WinMM, CoreMIDI, ALSA | Linux: libasound (ALSA sequencer) |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | persistence, app (dev) | временные каталоги в тестах | — |

Проверено: Claude Code, 2026-10-20, по `cargo metadata`.

## Решения и ограничения

- **ASIO (ADR-006, spec 0.2.2 ASIO-030).** Без crate `asio-sys` и без
  файлов ASIO SDK: интерфейс драйвера и структуры переписаны в
  `crates/audio-asio/src/abi.rs` и `com.rs` по заголовкам ASIO SDK
  (Steinberg; GPLv3 — один из вариантов лицензии SDK) и сверены с
  привязками asio-sys 0.4.0 (Apache-2.0). Уведомление —
  `packaging/windows/ASIO-NOTICE.txt`, ставится MSI рядом с программой.
  Версия SDK, с которой действует GPLv3, по первоисточнику не проверена
  [VERIFY]; название «ASIO» — без логотипа.

- **WAV (BLD-040).** `hound` не читает IEEE float 64-bit, поэтому
  в `persistence/src/audio_file.rs` есть небольшой собственный
  RIFF-reader только для этого subtype. Остальные subtype
  (PCM 8/16/24/32, float 32, `WAVE_FORMAT_EXTENSIBLE`) читает `hound`.
- **Ресемплинг (BLD-041).** Временная реализация: cubic Hermite без
  anti-aliasing фильтра, собственный код. Алгоритм для production
  выбирается в ADR-012 (кандидат: `rubato`).
- **MP3 (спецификация 0.2.0 §12).** Адаптер `crates/mp3` поверх
  LAME 3.100: CBR 128–320 кбит/с, стерео, 44,1/48 кГц, ID3 title/artist,
  info-фрейм LAME с encoder delay/padding. LAME — LGPL-2.0-or-later,
  совместима с GPLv3 приложения; исходники LAME входят в crate
  `mp3lame-sys`, сборка статическая, ffmpeg не нужен. Кросс-сборка на
  машине разработчика невозможна без C-тулчейнов целевой ОС: локальный
  `cargo clippy --target …` запускается с `DOCS_RS=1` (build script
  LAME пропускается); настоящая сборка — нативно в CI.
- **Rhai (спецификация 0.2.0 §18.22).** Тянет `smartstring`, помеченный
  RUSTSEC-2026-0249 как unmaintained (не уязвимость). Исключение в
  `deny.toml` с причиной; пересмотреть, когда Rhai от него откажется.
- **PipeWire.** В cpal 0.18 есть feature `pipewire` (нативный backend).
  Он не включён: по спецификации для MVP проверяется работа через
  pipewire-jack (AUD-001).
- **Не pure Rust.** Через cpal и eframe используются системные
  библиотеки (ALSA, JACK, OpenGL, xkbcommon, CoreAudio, WASAPI).
