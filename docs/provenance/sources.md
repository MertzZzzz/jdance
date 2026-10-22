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
- Для `docs/spec/spec-0.2.2.md` (2026-10-22, тоже по выдержкам поисковой
  выдачи): `html/playlist_audioclip.htm`, `chansettings_sampler.htm`,
  `recording_audio.htm`, `envsettings_audio.htm`, `app_wiz3.htm`,
  `mixer_mixermenu.htm`; ответы на форуме Image-Line о маршруте записанных
  клипов помечены в спецификации [?].
- Для `docs/spec/spec-0.2.3.md` (2026-10-22; прямое чтение руководства
  недоступно, только выдержки поисковой выдачи): `html/menu_options.htm` —
  пункты Metronome (Ctrl+M) и Recording precount (Ctrl+P). Остальное
  поведение в этом документе помечено [?] или [Н].
- ASIO SDK (Steinberg): лицензия GPLv3 с октября 2025 (сообщения
  Steinberg и прессы). Для `crates/audio-asio` (2026-10-22): раскладка
  интерфейса драйвера (порядок методов таблицы виртуальных функций),
  структуры `ASIOBufferInfo`, `ASIOChannelInfo`, `ASIOTime`,
  `ASIOCallbacks`, коды ошибок, селекторы `asioMessage` и типы сэмплов —
  по заголовкам SDK (`asio.h`, `iasiodrv.h`) и по привязкам, которые
  генерирует asio-sys 0.4.0 (crate cpal, Apache-2.0). Код SDK
  (`asiodrivers`, `asiolist`) не переносился: список драйверов читается из
  реестра своим кодом. Точная версия SDK и дата перехода на GPLv3 —
  [VERIFY] (ASIO-030).
- Скриншоты окон FL Studio, присланные пользователем 2026-10-21 (Mixer,
  Piano Roll, Channel Rack, Playlist): использованы только для перечня
  элементов окон; в репозиторий не добавлены, размеры, цвета и иконки
  с них не переносились (UI-VIS-002/003).

- Для `docs/spec/spec-0.2.5.md` (2026-10-22; спецификация заказчика,
  импортирована из его документа): страницы руководства Image-Line
  `html/playlist.htm`, `html/browser.htm`, `html/chansettings_sampler.htm`,
  `html/basics_interface.htm`, `html/mixer.htm` (целиком) и
  `html/menu_patterns.htm`, `html/chansettings_misc.htm`,
  `html/basics_shortcuts.htm` (выдержки); форум Image-Line, темы
  p=1783403, p=1013299, p=1987678 (пресеты имён). Скриншоты заказчика
  (окно сэмплера, окно цвета, окно имени паттерна) использованы только
  для перечня элементов; в репозиторий не добавлены. Палитра, таблица
  имён цветов, пресеты имён и названия режимов растяжения пишутся заново
  (CR spec-0.2.5 «Принципы clean room»).

Материалы из CR-002 (бинарники, ресурсы, внутренние документы FL Studio)
не используются.

## Ассеты

| Файл | Происхождение | Лицензия |
|---|---|---|
| `assets/demo/samples/*.wav` | синтезированы кодом `cargo xtask gen-demo` | GPL-3.0-or-later (как репозиторий) |
| `assets/demo/demo.jdance` | создан `cargo xtask gen-demo` | GPL-3.0-or-later |
| `crates/test-support/golden/*.wav` | результат рендера тестов | GPL-3.0-or-later |
