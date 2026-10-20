# Amendment 01: Rust, cross-platform, ASIO

Эти требования имеют приоритет над противоречащими пунктами
предыдущей версии SPEC.md.

## 1. Платформы MVP

Поддерживаемые платформы:
- Windows 11 x86_64;
- macOS Apple Silicon;
- Linux x86_64.

macOS Intel:
- архитектурно не запрещать;
- не объявлять поддерживаемой платформой без сборки и проверки.

Для macOS и Linux перед release зафиксировать:
- минимальную поддерживаемую версию ОС;
- reference machine;
- поддерживаемые аудиоконфигурации.

Для Linux указать конкретную reference distribution.
Не обещать совместимость со всеми дистрибутивами.

Все основные пользовательские сценарии MVP
должны работать на трёх платформах.

ASIO является Windows-specific integration.
Отсутствие ASIO на macOS/Linux не считается отсутствием
функциональной поддержки аудио.

## 2. Стек

Основной язык:
- Rust stable;
- edition и toolchain фиксируются в репозитории.

Сборка:
- Cargo workspace;
- Cargo.lock хранится в репозитории приложения;
- rust-toolchain.toml;
- cargo fmt;
- cargo clippy;
- cargo test.

Предлагаемый UI:
- egui/eframe;
- собственные canvas-компоненты для Piano Roll,
  Arrangement, Step Sequencer и meters.

Перед окончательным выбором UI:
- проверить canvas rendering;
- scrolling;
- zoom;
- HiDPI;
- drag-and-drop файлов;
- работу на всех целевых ОС.

Audio I/O:
- CPAL как первоначальный cross-platform adapter;
- отдельный собственный backend adapter там,
  где CPAL не закрывает acceptance criteria.

Persistence:
- serde;
- serde_json.

Декодирование WAV:
- выбрать Rust-библиотеку после проверки
  поддерживаемых WAV subtype и лицензии.

Тестовые и DSP-зависимости:
- выбирать по фактической необходимости;
- фиксировать версии и лицензии;
- не подключать крупный DSP framework без обоснования.

JUCE и CMake исключаются из основного стека.

Требование Rust-first не означает запрет системных API,
FFI и необходимых native build dependencies.
Каждую такую зависимость документировать.

Не заявлять pure Rust, если используются native bindings.

## 3. Cargo workspace

Предлагаемая структура:

```
crates/
  domain/
  commands/
  dsp/
  engine/
  audio-api/
  audio-cpal/
  persistence/
  ui/
  app/
  test-support/

platform/
  windows-asio-driver/

docs/
assets/
```

Назначение crates:

domain:
- проект;
- инструменты;
- паттерны;
- ноты;
- клипы;
- mixer state;
- стабильные ID.

commands:
- редактирование;
- undo/redo;
- dirty state.

dsp:
- sampler;
- synth;
- envelopes;
- gain/pan;
- mixing;
- resampling.

engine:
- transport;
- scheduler;
- voice lifecycle;
- playback state;
- offline rendering.

audio-api:
- собственные platform-neutral interfaces;
- описание устройств;
- stream configuration;
- capabilities;
- errors;
- diagnostics.

audio-cpal:
- адаптация CPAL к audio-api;
- платформенные особенности не выходят наружу.

persistence:
- формат проекта;
- assets;
- safe save;
- recovery;
- migrations.

ui:
- представление;
- ввод;
- editor state;
- отправка commands.

app:
- wiring;
- lifecycle;
- startup;
- shutdown.

platform/windows-asio-driver:
- отдельный компонент;
- отдельные build и acceptance criteria;
- не смешивать его с ASIO-host backend DAW.

domain, dsp и engine не должны зависеть от UI.

Offline rendering не должен зависеть
от доступности аудиоустройства.

## 4. Матрица аудиоподдержки

Windows:
- WASAPI как базовый путь работы без внешнего ASIO;
- ASIO как обязательный professional audio backend.

macOS:
- CoreAudio.

Linux:
- ALSA;
- JACK.

PipeWire:
- проверить сценарий работы через совместимый JACK server;
- не называть эту интеграцию native PipeWire backend;
- native PipeWire backend не обязателен для MVP.

Фактическую доступность возможностей проверять
по выбранной закреплённой версии библиотек.

Не включать feature flag и не считать backend готовым:
каждый обязательный backend должен пройти smoke test.

## 5. Audio settings

Экран настроек должен предоставлять:

- backend;
- device/driver;
- output channel mapping;
- sample rate;
- buffer configuration, если backend позволяет;
- фактически активную конфигурацию;
- reported input/output latency, если API её сообщает;
- состояние stream;
- диагностические ошибки.

Не предполагать:
- одинаковые возможности у всех backend;
- произвольный buffer size;
- одинаковый input/output channel count;
- поддержку любого sample rate.

UI должен строиться на capabilities.

Если значение нельзя изменить из приложения:
- показывать фактическое значение;
- объяснять ограничение;
- предлагать control panel только при наличии поддержки.

Настройки сохранять отдельно для каждого backend/device.

При недоступности сохранённого устройства:
- показать понятное уведомление;
- предложить выбор;
- не переключаться молча на неизвестный выход.

## 6. ASIO-host backend: Windows MVP

Назначение:
использовать сторонние установленные ASIO-драйверы,
включая драйверы Audient.

Требования:
- обнаружение доступных ASIO-драйверов;
- выбор драйвера пользователем;
- получение доступных каналов и sample rates;
- получение поддерживаемых buffer constraints;
- запуск и остановка playback;
- корректная обработка sample format;
- открытие driver control panel при поддержке;
- реакция на reset/reconfiguration requests;
- обработка недоступного или занятого драйвера;
- корректное освобождение ресурсов при закрытии.

Не считать Audient отдельным backend:
это сторонний драйвер внутри ASIO backend.

Не требовать отдельного vendor SDK,
если стандартного ASIO достаточно.

Приложение не должно:
- скачивать драйверы производителя без согласия;
- поставлять чужие драйверы без разрешения;
- обходить установку или проверки драйвера;
- обещать одновременную работу нескольких ASIO-устройств.

MVP:
- один активный audio backend;
- одно активное ASIO device;
- configurable stereo output pair;
- полный multi-device aggregation вне scope.

Audio recording остаётся вне scope MVP.
Не объявлять запись реализованной только потому,
что драйвер предоставляет input channels.

ASIO hardware acceptance:
- проверить минимум один реальный Audient interface;
- зафиксировать модель;
- версию драйвера;
- ОС;
- sample rate;
- buffer size;
- продолжительность проверки;
- обнаруженные ограничения.

Без реального устройства:
ставить NOT RUN, а не PASS.

## 7. Собственный ASIO-драйвер

Это отдельный deliverable, а не синоним audio engine.

До реализации требуется ADR:
- какую задачу решает драйвер;
- какие приложения являются его клиентами;
- откуда и куда идёт аудио;
- какой backend используется снизу;
- какие ограничения по latency и совместимости;
- нужен ли interprocess transport.

Рассматриваемые варианты:

A. ASIO-to-WASAPI bridge:
   ASIO-приложение → наш драйвер → WASAPI device.

B. Virtual ASIO endpoint:
   ASIO-приложение → наш драйвер → IPC/shared memory
   → наш audio service или DAW.

Не выбирать A или B молча:
это разные продукты и разные acceptance criteria.

Собственный драйвер не должен:
- подменять драйвер Audient;
- заявлять управление его аппаратными функциями;
- использовать чужое имя или регистрацию;
- включать kernel-mode component без отдельного обоснования;
- объявляться универсальным драйвером звуковых карт.

До принятия ADR:
- допускается scaffolding и исследовательский spike;
- production implementation не считается определённой.

Для выбранного варианта подготовить:
- ABI/FFI boundary;
- lifecycle;
- registration/unregistration;
- installer/uninstaller;
- architecture compatibility;
- failure handling;
- licensing review;
- manual test plan.

Driver callbacks:
- без panic unwind через FFI;
- без блокирующего IPC;
- с заранее выделенной памятью;
- с определённым поведением при underrun;
- с безопасной реакцией на отключение клиента.

Собственный драйвер не должен быть обязательным
для работы DAW через сторонний ASIO-драйвер.

Не помещать наш драйвер в цепочку Audient по умолчанию.

## 8. Realtime Rust constraints

Rust не отменяет realtime constraints.

В audio callback запрещены:
- heap allocation;
- освобождение heap-объектов;
- lock/wait;
- blocking channels;
- filesystem/network access;
- logging с неизвестным поведением;
- panic;
- рост Vec/String/HashMap;
- уничтожение последнего Arc с тяжёлым payload.

Использовать:
- заранее выделенные буферы;
- bounded queues;
- ограниченное число голосов;
- предсказуемую сложность;
- deferred reclamation вне callback.

Lock-free не означает автоматически:
- allocation-free;
- wait-free;
- bounded execution time.

Для каждой realtime structure зафиксировать эти свойства.

Unsafe:
- изолировать в небольших модулях;
- документировать safety invariants;
- не использовать для обхода borrow checker
  без описанной модели владения.

No panic across FFI.

Audio engine должен корректно обрабатывать
разный фактический размер callback buffer.

Не предполагать, что requested buffer size
всегда равен observed buffer size.

## 9. Cross-platform CI и проверка

На каждой целевой платформе:
- cargo fmt/check;
- cargo clippy;
- unit tests;
- integration tests;
- release build;
- headless offline render test.

Отдельно:
- сборка Windows ASIO feature;
- проверка native dependencies;
- GUI startup smoke test;
- manual audio smoke test.

CI без подключённого audio hardware
не подтверждает аппаратную совместимость.

Пакетирование:
- Windows: установщик или portable build;
- macOS: .app bundle;
- Linux: один выбранный release artifact.

Code signing, notarization и системные ограничения запуска
зафиксировать как отдельные release tasks.
Не утверждать, что они выполнены без проверки.

## 10. Обновление этапов

Milestone 0:
- Rust workspace;
- GUI window;
- CI на трёх ОС;
- лицензии;
- architecture ADR;
- решение по назначению собственного ASIO-драйвера.

Milestone 1:
- audible vertical slice на трёх ОС;
- WASAPI/CoreAudio/ALSA;
- WAV sampler;
- 16-step loop;
- audio settings.

Milestone 1A:
- Windows ASIO-host;
- сторонний установленный драйвер;
- Audient hardware test;
- driver control panel / documented limitations.

Milestone 1B:
- Linux JACK;
- проверка PipeWire-JACK configuration.

Последующие музыкальные milestone:
- сохранить из исходной спецификации.

Отдельная ветка deliverable:
- собственный ASIO-драйвер;
- prototype;
- integration;
- external-host acceptance;
- installer/uninstaller.

MVP нельзя объявлять полностью завершённым,
пока обязательный собственный ASIO deliverable
не определён и не прошёл согласованные проверки.
