# Спецификация 0.2.0 — Audio Clips, Recording and Editing Workflow

## 1. Статус и цель релиза

Версия 0.1.0 считается существующей рабочей базой.

Перед реализацией:
- изучить текущую архитектуру и формат проекта;
- запустить существующие тесты;
- зафиксировать baseline;
- сопоставить требования с реальными модулями.

Не переписывать работающий engine или UI целиком.

Новые требования имеют приоритет над ограничениями 0.1.0, исключавшими audio clips, recording и automation.

### Платформы и стек

- Windows, Linux, macOS.
- Существующий Rust workspace.
- Native dependencies и FFI допускаются с документированием и лицензионной проверкой.

### Основные функции

1. Audio clips непосредственно в Playlist.
2. Drag-and-drop сэмплов.
3. Sample Editor.
4. Изменение границ clips с обоих краёв.
5. Пользовательские папки сэмплов.
6. Пользовательские папки плагинов.
7. Запись выбранного аппаратного входа.
8. Инструменты Draw/Paint/Delete/Slice.
9. Dock/Undock рабочих панелей.
10. Project Browser: Patterns / Samples / Automations.
11. Автоматизация параметров.
12. MP3 export.

### Вне scope

- Time stretching и pitch-preserving tempo matching.
- Warp markers.
- Comping и take lanes.
- Punch recording.
- Loop recording с несколькими takes.
- Multi-device clock synchronization.
- Destructive audio editing.
- Полноценный VST/AU/CLAP hosting, если его нет в существующем приложении.

## 2. Audio Clips непосредственно в Playlist

Исходный сэмпл и размещение на timeline — разные сущности. Один файл может использоваться несколькими clips с разными границами.

### AudioAsset

- стабильный AssetId;
- display name;
- project-relative file path;
- content hash;
- source sample rate;
- channel count;
- source frame count;
- metadata/cache state.

### AudioClip

- стабильный ClipId;
- AssetId;
- timeline start;
- source in frame;
- source out frame;
- gain;
- muted;
- fade-in duration;
- fade-out duration.

Source range: `[source_in_frame, source_out_frame)`.

Ограничения:
- границы неотрицательны;
- source out > source in;
- диапазон не выходит за длину AudioAsset.

Длина AudioClip определяется source range и исходным sample rate. Не хранить независимую длину, противоречащую source range.

AudioClip:
- не зависит от InstrumentChannel;
- не требует Pattern;
- не транспонируется;
- не меняет скорость при изменении BPM.

### Временная модель

PatternClip хранит позицию и длину в ticks.

AudioClip хранит позицию на timeline в целых project frames.

- Project timebase sample rate задаётся при создании проекта и сохраняется.
- Device sample rate не меняет сохранённые позиции.
- Project timebase не обязан совпадать с device sample rate.
- При snap позиция в ticks преобразуется в project frames.

При изменении BPM:
- AudioClip сохраняет абсолютное положение и длительность;
- его отображение относительно bar grid может измениться;
- PatternClip сохраняет музыкальное положение.

Это поведение явно описать в UI/help.

При различии device rate и project timebase engine выполняет соответствующее отображение времени.

При различии source и render sample rates использовать resampler. Не воспроизводить исходные frames как frames устройства напрямую.

### Добавление

Поддержать drag-and-drop:
- из Samples Browser;
- из Project Samples;
- из файлового менеджера ОС.

Drop на свободное место:
- создаёт AudioClip;
- использует текущий snap;
- импортирует asset в проект;
- не создаёт Pattern или InstrumentChannel.

Drop в область заголовков/пустых lanes создаёт lane при необходимости.

Во время drag показывать положение, длительность и имя файла.

Импорт и waveform generation выполняются вне audio thread.

Ошибка импорта не оставляет сломанный clip и сопровождается понятным сообщением.

### Routing

Каждая Playlist lane получает Audio Output Bus для размещённых на ней AudioClips.

Этот bus:
- применяется только к AudioClips;
- не переопределяет routing инструментов внутри PatternClips.

Заголовок lane явно отражает различие. Default bus: Master.

Overlapping AudioClips суммируются.

Seek внутрь AudioClip начинает воспроизведение с соответствующей source position, не дожидаясь следующего начала клипа.

### Границы воспроизведения

На source out:
- прекратить чтение за пределами диапазона;
- обработать fade-out;
- не воспроизводить скрытый хвост файла.

Default anti-click fades: до 3 ms на каждом краю с ограничением по длительности clip. Fade не меняет source data.

## 3. Двойной клик и Sample Editor

### Действия двойного клика

| Объект | Действие |
|---|---|
| PatternClip | Открыть Pattern Editor и Piano Roll последнего выбранного инструмента; при отсутствии выбора показать selector |
| AudioClip | Открыть Sample Editor и выделить source range clip |
| AudioAsset в Project Samples | Открыть Sample Editor для всего asset |
| Файл в папках сэмплов | Открыть preview/Sample Editor без автоматического добавления в Playlist |
| AutomationClip | Открыть Automation Editor |

Не открывать пустой Piano Roll для AudioAsset. Piano Roll редактирует ноты инструмента, использующего сэмпл, а не сам аудиофайл.

### Sample Editor

Поддержать:
- waveform;
- zoom;
- horizontal scroll;
- play/stop preview;
- selection;
- duration/sample rate/channels;
- source range выбранного AudioClip;
- изменение source in/out;
- Place in Playlist;
- Load into Sampler.

Редактирование AudioClip меняет только этот clip, не изменяет исходный файл и другие clips с тем же AssetId.

Load into Sampler создаёт либо обновляет выбранный sampler после явного выбора пользователя. Далее ноты редактируются в Piano Roll.

Destructive processing и перезапись оригинала вне scope.

## 4. Изменение длины за левый и правый края

Изменение длины по умолчанию — недеструктивная обрезка/раскрытие, не time stretching.

Левый и правый края имеют отдельные hit zones и resize cursor.

Для узкого clip:
- определять ближайший край;
- сохранять возможность выбрать body;
- обеспечивать работу после zoom.

Drag resize — одна undo operation.

Snap применяется к новой позиции края. Modifier временного отключения snap соответствует общей keymap.

### AudioClip: правый край

- Вправо: раскрывает скрытое содержимое до source file end.
- Влево: уменьшает source out.
- Timeline start не меняется.
- Stretch и loop автоматически не включаются.

### AudioClip: левый край

Вправо:
- увеличивает source in;
- переносит timeline start вправо;
- сохраняет timeline end.

Влево:
- раскрывает предыдущее source content;
- переносит timeline start влево;
- сохраняет timeline end.

Нельзя раскрывать source position < 0.

### PatternClip: обновление модели

Добавить:
- source_offset_ticks;
- duration_ticks;
- loop_enabled.

Удалить repeat_count как независимый источник истины. Количество повторов вычисляется.

Loop off:
- clip показывает диапазон Pattern;
- раскрытие ограничено границами Pattern.

Loop on:
- содержимое повторяется с периодом Pattern length;
- duration — произвольная положительная;
- допускается неполное последнее повторение.

Правый край меняет duration, не изменяя Pattern definition.

Левый край меняет start, source offset и duration, сохраняя timeline end и положение уже видимого содержимого.

Отрицательные source offsets не хранить. Для loop использовать нормализованный phase offset.

### Notes

- Левый край меняет start и duration, сохраняя end.
- Правый край меняет duration.
- Минимальная длительность — 1 tick.

### Миграция PatternClip

Для старого clip:
- source_offset = 0;
- duration = old repeat_count × pattern length;
- loop_enabled = true.

Сохранить прежнее воспроизведение проекта.

## 5. Папки сэмплов и выбор из библиотеки

### Настройки библиотеки

Settings → Library → Sample Folders.

Поддержать:
- добавить папку;
- удалить папку из библиотеки;
- переименовать отображаемый root;
- включить/выключить recursive indexing;
- rescan.

Удаление root не удаляет файлы.

Папки библиотеки — глобальные настройки приложения, не обязательная зависимость portable project. Project assets продолжают копироваться в проект.

### Browser

Поддержать:
- дерево папок;
- поиск по имени;
- фильтр поддерживаемых форматов;
- preview;
- waveform preview по готовности;
- drag-and-drop;
- контекстное меню.

Не загружать всю библиотеку в RAM.

Индексирование:
- background;
- cancelable;
- progress;
- без зависания UI;
- без циклического обхода symlinks.

### Sample Selection Dialog

Во всех местах выбора сэмпла предоставить:
1. Library.
2. Project Samples.
3. File System.

Кнопки: Preview, Select, Cancel.

Выбор из библиотеки работает для sampler и AudioClip.

Отсутствующие roots помечаются unavailable, не удаляются автоматически и не ломают проекты.

## 6. Пользовательские папки плагинов

Управление путями и обнаружение плагинов не означают полноценный hosting.

Settings → Plugins → Search Paths.

Поддержать:
- системные и пользовательские пути;
- add/remove;
- enable/disable;
- rescan;
- scan progress;
- cancel;
- список ошибок.

Первый поддерживаемый формат каталога: VST3.

Не считать произвольный .dll/.so/.dylib VST3-плагином.

### PluginRecord

- format;
- plugin class ID;
- module path;
- name;
- vendor;
- category;
- architecture;
- scan status;
- error;
- module fingerprint.

Один module может содержать несколько plugin classes.

### Scanner

Сканирование plugin code выполняется в отдельном worker process.

Для каждого module:
- timeout;
- crash detection;
- error capture;
- quarantine после сбоя.

Не повторять crash scan бесконечно при каждом запуске.

Не сканировать в audio callback и не блокировать основной UI.

### Scope Gate

Если hosting отсутствует в 0.1.0:
- каталогизация обозначена discovery only;
- Insert неактивна;
- фиктивные processors не создаются.

Если hosting уже есть:
- интегрировать paths с существующим registry;
- проверить instantiation, state persistence, missing plugin handling.

AU, VST2 и CLAP не объявлять поддерживаемыми без реализации.

## 7. Запись микрофона и линейного входа

Поддержать один input route:
- mono: один аппаратный канал;
- stereo: выбранная пара.

Пользователь выбирает:
- input device/backend;
- physical channel mapping;
- target Playlist lane;
- monitoring on/off.

Не угадывать назначение входа по номеру. Использовать driver-provided names либо Input 1, Input 2 и т.д.

### Ограничения устройств

Использовать одну согласованную input/output configuration.

Если backend не поддерживает duplex-конфигурацию:
- показать ошибку;
- не создавать скрытую multi-device aggregation.

Независимые устройства с разными clocks вне scope.

### Workflow

1. Выбрать input.
2. Arm target lane.
3. Проверить input meter.
4. Установить playhead.
5. Нажать Record.
6. Transport запускает playback и capture.
7. Stop завершает запись.
8. Создаются AudioAsset и AudioClip.

Monitoring:
- default off;
- включается явно;
- предупреждает об обратной связи;
- не включает passthrough скрыто.

Armed input meter работает без monitoring.

### Запись файла

Формат: WAV float32, mono/stereo.

Audio callback помещает frames в заранее выделенный bounded buffer. Writer thread пишет файл, завершает WAV header и регистрирует asset.

Никакого file I/O в audio callback.

При overflow:
- остановить recording с сообщением;
- сохранить валидную часть;
- не продолжать молча с потерянными frames.

При disk full завершить доступную валидную часть и сообщить об ошибке.

### Позиционирование

Запись привязывается к capture timeline, не ко времени завершения записи файла.

Использовать timestamps/latency data backend, если доступны.

Поддержать manual recording offset calibration.

Не вычитать reported latency повторно, если timestamp mapping уже её учитывает.

Без измерений не заявлять sample-accurate hardware alignment.

### Ограничения

- Один target lane за запись.
- Без punch, takes и loop recording.
- При активном loop предложить отключить его либо отменить запись.

### Recovery

Обнаруживать незавершённые recording files при запуске.

Предложить восстановить, сохранить отдельно или удалить после подтверждения.

Undo recorded clip убирает clip, но не уничтожает файл немедленно.

Разрешения микрофона и отказ ОС обрабатываются на всех платформах.

## 8. Toolbar: Draw, Paint, Delete, Slice

Каждый инструмент имеет собственную иконку, tooltip, active state и shortcut из общей keymap.

Не копировать оригинальные иконки FL Studio.

Инструмент действует на focused editor.

### Draw

Piano Roll:
- click на пустом месте создаёт одну ноту;
- drag после создания задаёт длину;
- drag существующей ноты перемещает её;
- edge drag выполняет resize.

Playlist:
- click создаёт один экземпляр выбранного Pattern/AudioAsset/Automation source;
- drag после создания перемещает его;
- не создаёт цепочку копий.

Без source показать подсказку, ничего не создавать.

### Paint

Piano Roll:
- рисует ноты в последовательно посещаемых grid cells;
- вертикальная позиция задаёт pitch;
- одна cell не создаёт дубликаты в одном gesture.

Playlist:
- рисует последовательные неперекрывающиеся экземпляры source;
- следующий начинается в конце предыдущего;
- первый использует snap.

При быстром движении заполнять пропущенные позиции между pointer events.

Один gesture — одна undo operation.

Повторы audio — отдельные AudioClips, не stretch.

### Delete

Click удаляет объект. Drag удаляет объекты, пересечённые курсором.

Один gesture — одна undo operation.

Удаление clip не удаляет source asset или Pattern definition.

### Slice

Piano Roll:
- разрезает ноту;
- создаёт две независимые ноты;
- сохраняет pitch и velocity;
- на playback это retrigger, не legato.

PatternClip:
- создаёт два clips;
- сохраняет Pattern reference;
- корректирует source offset правой части;
- не меняет Pattern.

AudioClip:
- создаёт два clips с общим AssetId;
- разделяет source range;
- не создаёт новые файлы;
- не меняет arrangement duration.

Обе части используют одну source frame boundary, без потери/дублирования frames.

Разрез на краю — no-op. Объекты нулевой длины запрещены.

Slice automation вне scope.

## 9. Dock / Undock

Поддержать для:
- Piano Roll;
- Sample Editor;
- Automation Editor;
- Mixer;
- Project Browser.

Playlist — основная область.

Detached panel — отдельное окно той же session, не копия domain model.

Закрытие окна скрывает panel, не закрывает проект и не удаляет данные.

Поддержать:
- Dock back;
- Reset layout;
- сохранение расположения и размеров;
- multi-monitor;
- HiDPI.

Если монитор недоступен, переместить окно в видимую область.

Layout хранится в user settings.

Shortcuts действуют на focused editor. Ввод текста не запускает editing shortcuts. Правила transport shortcuts определены явно.

При необходимости native viewport support сначала выполнить cross-platform spike. Не имитировать undock прямоугольником внутри главного окна.

## 10. Project Browser слева от Playlist

Три вкладки:
1. Patterns.
2. Samples.
3. Automations.

Это объекты проекта, не библиотека файлов на диске.

Общие функции:
- search;
- selection;
- rename;
- context menu;
- drag to Playlist;
- usage count;
- add/delete.

### Patterns

- add empty;
- duplicate;
- rename;
- delete;
- double-click → Pattern/Piano Roll;
- drag → PatternClip.

При удалении используемого Pattern показать число зависимых clips и предложить cancel либо удаление Pattern с clips. Операция undoable.

### Samples

Список AudioAssets проекта:
- import from library/file;
- rename display name;
- preview;
- double-click → Sample Editor;
- drag → AudioClip;
- Load into Sampler;
- delete.

Rename не переименовывает внешний оригинал.

Удаление учитывает AudioClips и sampler references. Используемый asset не удаляется молча. Внешний оригинал не удаляется.

### Automations

- create;
- duplicate;
- rename;
- delete;
- double-click → Automation Editor;
- drag → AutomationClip.

Показывать target, missing target state и usage count.

## 11. Минимальная автоматизация

### AutomationSource

- ID;
- name;
- target ParameterAddress;
- length_ticks;
- points.

### AutomationPoint

- ID;
- position_ticks;
- normalized_value: 0..1;
- interpolation: step | linear.

### AutomationClip

- ID;
- source ID;
- start_tick;
- source_offset_ticks;
- duration_ticks.

В 0.2.0 без loop. Clip range ограничен длиной source.

### Targets

Минимум:
- mixer bus gain;
- mixer bus pan/balance;
- AudioClip gain.

Effect parameters поддерживать только при наличии stable parameter IDs и realtime-safe updates.

Plugin automation не обещать без hosting.

ParameterAddress использует stable IDs, не индексы/имена.

Normalized value преобразуется через mapping параметра.

### Evaluation

Automation действует только внутри активного clip.

Внутри source:
- до первой точки удерживать первую;
- после последней удерживать последнюю;
- между точками — выбранная интерполяция.

Без активной automation использовать manual base value.

При seek сразу вычислять значение в новой позиции.

При mute/delete вернуться к manual value с smoothing.

Пересечение активных AutomationClips одного target запрещено. Конфликтующее размещение отклонять с сообщением.

### Editor

- add/move/delete points;
- snap;
- step/linear;
- zoom;
- undo/redo.

Применять значения внутри audio block по frame offsets или ramps, не с частотой UI.

## 12. MP3 Export

Format selector: WAV / MP3.

Настройки MP3:
- 44.1 / 48 kHz;
- CBR: 128 / 192 / 256 / 320 kbps;
- default: 320 kbps;
- stereo;
- optional title/artist.

VBR вне обязательного scope.

Pipeline:

```text
offline render → PCM blocks → encoder → temporary file
→ finalize → atomic publish where supported
```

- Не кодировать в audio callback.
- Не использовать loopback recording.
- Использовать тот же engine/snapshot, что и WAV.
- Показывать progress/cancel/error/completed path.
- После cancel/error не оставлять файл под видом успешного export.

Предлагаемый encoder: LAME через отдельный adapter.

Зафиксировать версию, лицензию, способ линковки и native packaging на всех ОС.

Не требовать установленный ffmpeg без отдельного решения о поставке.

Не добавлять скрытую нормализацию. Предупреждать о перегрузке PCM. Сохранять действие включённого master limiter.

Учитывать encoder delay/padding при проверке decoded output. Не требовать byte-identical duration с PCM.

## 13. Миграция и сохранность

Увеличить schema version.

Добавить:
- AudioAssets metadata;
- AudioClips;
- lane audio routing;
- PatternClip source offset/duration/loop;
- AutomationSources;
- AutomationClips.

Проекты 0.1.0:
- открываются без потери событий;
- сохраняют расположение/повторы;
- не перезаписываются при ошибке миграции.

Перед первым сохранением мигрированного проекта создать backup manifest.

User settings:
- sample roots;
- plugin roots;
- scan cache;
- workspace layout.

Не сохранять plugin binaries в assets.

Orphan assets не удалять автоматически. Предоставить explicit cleanup operation.

## 14. Этапы реализации

| Этап | Результат | Проверка |
|---|---|---|
| 0.2-A | AudioAsset, AudioClip, waveform, drag-and-drop | WAV размещается и звучит без Pattern |
| 0.2-B | Resize, offsets, Slice | Обрезка/разрез не меняют оригинал |
| 0.2-C | Sample Library, Project Browser | Выбор из папок без системного диалога |
| 0.2-D | Toolbar | Все tools работают с undo |
| 0.2-E | Recording | Выбранный вход записывается в правильной позиции |
| 0.2-F | Automation | Gain/pan воспроизводятся и экспортируются |
| 0.2-G | Dock/Undock | Работает на трёх ОС с восстановлением layout |
| 0.2-H | Plugin paths/scanner | Сбой scan не закрывает DAW |
| 0.2-I | MP3 export | Валидный полный render |

## 15. Сквозной acceptance scenario

1. Открыть проект 0.1.0.
2. Проверить старую аранжировку.
3. Добавить папку сэмплов.
4. Перетащить WAV в Playlist.
5. Открыть его двойным кликом.
6. Обрезать оба края.
7. Разрезать Slice.
8. Проверить undo/redo.
9. Нарисовать последовательность PatternClips через Paint.
10. Изменить длину PatternClip без изменения Pattern.
11. Выбрать input channel.
12. Записать несколько секунд.
13. Проверить clip и файл.
14. Создать mixer gain automation.
15. Отцепить Sample Editor и вернуть обратно.
16. Добавить plugin path и выполнить scan.
17. Сохранить и повторно открыть проект.
18. Экспортировать WAV и MP3.
19. Проверить audio clips, recording и automation в обоих renders.

Для hardware recording указать фактические backend/device. Synthetic input tests не подтверждают hardware test.

## 16. Правила работы Claude Code

Первое задание — аудит кода и план 0.2-A, не одновременная реализация всех функций.

Для каждого этапа:
1. Изучить существующие модули.
2. Описать изменения и риски.
3. Реализовать рабочий вертикальный срез.
4. Добавить unit/integration tests.
5. Выполнить build/test.
6. Обновить status и ограничения.
7. Указать реально проведённые и непроведённые проверки.

Не объявлять успешными невыполненные сборки/тесты.
Не выдавать mock UI за рабочую функцию.
Не менять scope молча.

## 17. Источники для реализации внешних интеграций

- VST3 API: https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/API+Documentation/Index.html
- VST Module Architecture: https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/VST+Module+Architecture/Index.html
- LAME: https://lame.sourceforge.io/index.php
- LAME licensing guidance: https://lame.sourceforge.io/license.txt

Документацию и действующие лицензии проверить при выборе конкретных версий зависимостей. Публичная доступность документации не разрешает произвольное копирование кода или ресурсов.
