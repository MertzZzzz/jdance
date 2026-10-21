# Источники и provenance (CR-003, CR-006, LIC-008)

## Источники требований к поведению

- Собственная спецификация: `docs/spec/`.
- Общие знания о DAW и публичная документация форматов
  (RIFF/WAVE, VST3 SDK, ASIO SDK — по мере подключения).

- Публичное руководство пользователя FL Studio (Image-Line) — источник
  требований к *поведению и составу* окон для `docs/spec/ui-visual-spec.md`
  (CR-003). 2026-10-21 прямое чтение страниц из среды разработки было
  закрыто сетевой политикой; использованы выдержки поисковой выдачи по
  официальным страницам (неподтверждённое помечено в спецификации [?]):
  - `html/mixer.htm`, `mixer_mixermenu.htm`, `mixer_trackprops.htm`,
    `mixer_plugin.htm`, `mixer_iorouting.htm`, `mixer_levelsandmixing.htm`;
  - `html/pianoroll.htm`, `pianoroll_menu.htm`, `pianoroll_qnt.htm`,
    `automation_eventeditor.htm`;
  - `html/channelrack.htm`, `chansettings_misc.htm`, `chansettings_layer.htm`;
  - `html/playlist.htm`, `toolbar_panels.htm`, `toolbar_edit.htm`,
    `basics_shortcuts.htm`, `menu_view.htm`, `menu_patterns.htm`,
    `recording_audio.htm`
  (все — под `https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/`).
- Скриншоты окон FL Studio, присланные пользователем 2026-10-21 (Mixer,
  Piano Roll, Channel Rack, Playlist): использованы только для перечня
  элементов окон; в репозиторий не добавлены, размеры, цвета и иконки
  с них не переносились (UI-VIS-002/003).

Материалы из CR-002 (бинарники, ресурсы, внутренние документы FL Studio)
не используются.

## Ассеты

| Файл | Происхождение | Лицензия |
|---|---|---|
| `assets/demo/samples/*.wav` | синтезированы кодом `cargo xtask gen-demo` | GPL-3.0-or-later (как репозиторий) |
| `assets/demo/demo.jdance` | создан `cargo xtask gen-demo` | GPL-3.0-or-later |
| `crates/test-support/golden/*.wav` | результат рендера тестов | GPL-3.0-or-later |
