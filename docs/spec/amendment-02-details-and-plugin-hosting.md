# Amendment 02: детализация Amendment 01 и хостинг плагинов (VST3) в MVP

Статус: DRAFT.

Приоритет: этот документ уточняет Amendment 01 и имеет приоритет
над противоречащими пунктами Amendment 01 и SPEC.md.
Если пункт здесь не упомянут, действует Amendment 01.

Нумерация разделов 1–10 совпадает с Amendment 01.
Новые разделы: 0, 11–16.

Пометка **[VERIFY]** означает факт о сторонней библиотеке, SDK
или лицензии, который нужно подтвердить по закреплённой версии
в Milestone 0. До подтверждения на нём нельзя строить
acceptance criteria.

---

## 0. Общие правила документа

### 0.1. Идентификаторы требований

Каждое проверяемое требование получает ID вида `AREA-NNN`:

| Префикс | Область |
|---|---|
| `PLAT` | платформы |
| `BLD`  | сборка, toolchain, зависимости |
| `ARCH` | архитектура и границы crates |
| `AUD`  | audio backends и settings |
| `ASIO` | ASIO-host backend |
| `DRV`  | собственный ASIO-драйвер |
| `RT`   | realtime constraints |
| `PLG`  | хостинг плагинов |
| `PER`  | persistence |
| `CI`   | CI, тесты, пакетирование |
| `CR`   | clean-room и provenance |

ID не переиспользуются. Удалённое требование помечается `WITHDRAWN`.

### 0.2. Статусы проверки

Для каждого acceptance-пункта допустимы только статусы:

- `PASS`: проверено на зафиксированной конфигурации, есть артефакт
  (лог, отчёт, запись);
- `FAIL`: проверено, требование не выполнено;
- `NOT RUN`: проверка не проводилась (нет железа, плагина, ОС);
- `BLOCKED`: проверку нельзя провести из-за внешней причины,
  причина указана;
- `WAIVED`: требование явно снято для конкретного release
  с подписью владельца продукта и причиной.

`PASS` без артефакта не засчитывается.
`NOT RUN` и `BLOCKED` не засчитываются как `PASS`
при решении о готовности MVP.

### 0.3. Формат acceptance-отчёта

Каждый отчёт фиксирует:
- commit hash;
- ОС и её версию;
- reference machine (CPU, RAM);
- audio backend, device, версию драйвера;
- sample rate и фактический (observed) buffer size;
- плагины и их версии, если использовались;
- длительность проверки;
- число xrun/underrun и способ их подсчёта;
- результат по каждому ID;
- обнаруженные ограничения.

Шаблон: `docs/acceptance/TEMPLATE.md`.

---

## 1. Платформы MVP: детализация

**PLAT-001.** Целевые triples MVP:
- `x86_64-pc-windows-msvc`;
- `aarch64-apple-darwin`;
- `x86_64-unknown-linux-gnu`.

**PLAT-002.** Windows on ARM, 32-bit Windows и `*-windows-gnu`
не поддерживаются и не собираются в release.

**PLAT-003.** macOS Intel (`x86_64-apple-darwin`): сборка в CI
разрешена, но платформа считается неподдерживаемой, пока
не пройден полный acceptance на реальной Intel-машине.
Universal binary не обязателен для MVP.

**PLAT-004.** Кандидаты на минимальные версии (утверждаются
в Milestone 0, затем меняются только через ADR):
- Windows: Windows 11 22H2+;
- macOS: определить по минимальной версии, которую поддерживают
  выбранные версии winit/wgpu и VST3 SDK; кандидат: macOS 13;
- Linux: одна reference distribution, кандидат: Ubuntu 24.04 LTS
  (glibc 2.39, PipeWire по умолчанию с pipewire-jack).

**PLAT-005.** Для Linux дополнительно фиксируется:
- display server: X11 и/или Wayland (см. PLG-062 про окна плагинов);
- audio stack reference: PipeWire + pipewire-jack и
  отдельно чистый ALSA без звукового сервера, если ALSA
  объявлен поддерживаемым напрямую.

**PLAT-006.** Reference machine на каждую ОС описывается в
`docs/platforms/<os>.md`: CPU, RAM, аудиоинтерфейс, версия ОС,
версия драйвера. Все performance-цифры в спецификации относятся
к reference machine.

**PLAT-007.** «Основной пользовательский сценарий MVP» = список
сценариев в `docs/scenarios.md`. Минимум:
1. запуск, создание проекта;
2. загрузка WAV в sampler-канал;
3. программирование step-паттерна и нот в Piano Roll;
4. загрузка VST3-инструмента и VST3-эффекта (см. раздел 11);
5. воспроизведение с изменением tempo;
6. сведение в mixer;
7. сохранение, закрытие, повторное открытие без потерь;
8. offline render в WAV;
9. смена audio device без перезапуска приложения.

Каждый сценарий проходит на трёх платформах.

---

## 2. Стек: детализация

### 2.1. Toolchain

**BLD-001.** `rust-toolchain.toml` фиксирует конкретную версию
(`channel = "1.xx.y"`, а не `stable`), components
`rustfmt`, `clippy` и targets из PLAT-001.

**BLD-002.** Edition: 2024 [VERIFY: совместимость выбранных
зависимостей]. MSRV = зафиксированная версия toolchain;
`rust-version` указан в workspace `Cargo.toml`.

**BLD-003.** Обновление toolchain делается отдельным PR
с полным прогоном CI на трёх ОС.

### 2.2. Политика зависимостей

**BLD-010.** Каждая зависимость первого уровня записывается
в `docs/dependencies.md`: crate, версия, лицензия, назначение,
native-компоненты, кто и когда проверил.

**BLD-011.** Допустимые лицензии без отдельного review:
MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-DFS/Unicode-3.0.
Всё остальное, включая MPL-2.0, LGPL, GPL и проприетарные SDK,
проходит license review до merge.

**BLD-012.** В CI работает `cargo deny` (licenses, bans, advisories,
sources). Конфигурация хранится в репозитории.

**BLD-013.** Native build dependencies (C/C++ компилятор,
`libclang` для bindgen, SDK-заголовки, pkg-config пакеты)
перечисляются для каждой ОС в `docs/building.md`.
Сборка с чистой машины по этому документу проверяется в CI.

**BLD-014.** Сторонние SDK (ASIO, VST3) не коммитятся в репозиторий,
если их лицензия это запрещает. Если лицензия разрешает
вендоринг, SDK кладётся в `third_party/` с исходной лицензией,
версией и хешем архива.

### 2.3. UI: критерии выбора

**BLD-020.** UI spike (Milestone 0) отвечает на вопросы с измеримыми
порогами на reference machine:
- Piano Roll с 10 000 нот: прокрутка и zoom без пропуска кадров
  при 60 Hz (p95 frame time ≤ 16.6 ms);
- Playlist с 500 клипами: тот же порог;
- корректный HiDPI: масштаб 100/150/200 % на Windows,
  Retina на macOS, fractional scaling на Linux;
- drag-and-drop WAV из файлового менеджера на всех ОС;
- **создание отдельного native-окна с raw window handle,
  в которое можно встроить GUI VST3-плагина** (см. PLG-060).
  Это блокирующий критерий выбора UI-стека.

**BLD-021.** Если egui/eframe не проходит хотя бы один порог,
решение фиксируется в ADR с альтернативой. Отказ от egui
не обязателен, если проблема решается собственным
canvas-компонентом.

**BLD-022.** UI thread не выполняет блокирующие операции дольше
50 ms (декодирование, scan, save). Они уходят в worker threads
с индикатором прогресса.

### 2.4. Audio I/O

**BLD-030.** CPAL фиксируется конкретной версией. Для этой версии
в `docs/audio/cpal-capabilities.md` составляется таблица:
backend × (enumeration, sample rate list, buffer size range,
latency reporting, device change notification, control panel,
reset request). Решения по собственным адаптерам принимаются
по этой таблице.

**BLD-031.** Предварительная оценка, которую нужно подтвердить
[VERIFY]:
- ASIO в CPAL доступен через feature `asio` и требует ASIO SDK
  и `libclang`;
- CPAL не даёт API для открытия ASIO control panel и обработки
  `kAsioResetRequest`/`kAsioBufferSizeChange`.

Если это подтвердится, ASIO-host реализуется собственным адаптером
`crates/audio-asio` поверх ASIO SDK (раздел 6), а не через CPAL.

### 2.5. WAV и ресемплинг

**BLD-040.** Минимальный набор WAV для MVP:
- PCM 16/24/32-bit integer;
- IEEE float 32/64-bit;
- mono и stereo;
- `WAVE_FORMAT_EXTENSIBLE`;
- sample rates 22.05–192 kHz.

Файлы вне набора отклоняются с понятной ошибкой без crash.
Кандидаты: `hound`, `symphonia` [VERIFY: subtypes, лицензия].

**BLD-041.** Ресемплинг сэмплов при загрузке (не в callback)
до project sample rate. Алгоритм и качество (например,
sinc с заданным passband) фиксируются в ADR. Кандидат:
`rubato` [VERIFY].

---

## 3. Cargo workspace: детализация

Обновлённая структура (новое помечено `+`):

```
crates/
  domain/
  commands/
  dsp/
  engine/
  audio-api/
  audio-cpal/
+ audio-asio/            Windows-only, собственный ASIO-host
+ plugin-api/            platform-neutral интерфейсы хостинга плагинов
+ plugin-vst3/           VST3 host implementation
+ plugin-scanner/        отдельный бинарник out-of-process сканера
  persistence/
  ui/
  app/
  test-support/
+ rt-check/              инструменты проверки realtime-инвариантов

platform/
  windows-asio-driver/

+ test-plugins/          собственные тестовые VST3-плагины (раздел 11.10)

docs/
  adr/
  acceptance/
  platforms/
assets/
third_party/             только если лицензии позволяют (BLD-014)
```

**ARCH-001.** Граф зависимостей (стрелка означает «может зависеть от»):

```
app → ui, engine, persistence, audio-*, plugin-*
ui → domain, commands, plugin-api (только типы), audio-api (только типы)
commands → domain
engine → domain, dsp, audio-api, plugin-api
plugin-vst3 → plugin-api
audio-cpal, audio-asio → audio-api
persistence → domain
dsp → (ничего из workspace, кроме rt-check в dev/test)
```

Запрещено:
- `domain`, `dsp`, `engine` → `ui`, `eframe`, `egui`, `winit`;
- `engine` → `plugin-vst3` напрямую (только через `plugin-api`);
- `engine` → `audio-cpal`/`audio-asio` напрямую.

Проверяется в CI скриптом по `cargo metadata`.

**ARCH-002.** `engine` можно собрать и протестировать без
аудиоустройства и без GUI: offline render работает
в headless CI на всех трёх ОС.

**ARCH-003.** Связь UI ↔ engine:
- UI → engine: команды через bounded SPSC/MPSC очередь;
- engine → UI: snapshot состояния (playhead, meters, voice count,
  CPU load, xrun counter) через triple buffer или seqlock;
- UI никогда не держит ссылку на структуры, которые читает
  audio thread.

**ARCH-004.** Модель «document → render graph»:
- `domain` хранит редактируемый документ;
- при изменении документа non-RT thread компилирует
  неизменяемый `RenderGraph`/`PlaybackSnapshot`;
- готовый snapshot передаётся в audio thread через lock-free swap;
- старый snapshot возвращается на освобождение в non-RT thread
  (deferred reclamation).

---

## 4. Матрица аудиоподдержки: детализация

**AUD-001.** Матрица MVP:

| ОС | Backend | Режим | Реализация | Обязателен для MVP |
|---|---|---|---|---|
| Windows | WASAPI shared | output | CPAL | да |
| Windows | WASAPI exclusive | output | CPAL или свой адаптер [VERIFY] | нет (желателен) |
| Windows | ASIO | output | `audio-asio` | да |
| macOS | CoreAudio | output | CPAL | да |
| Linux | ALSA | output | CPAL | да |
| Linux | JACK | output | CPAL `jack` feature [VERIFY] | да |
| Linux | PipeWire через pipewire-jack | output | тот же JACK backend | проверка сценария |

**AUD-002.** Smoke test backend'а = 10 минут непрерывного playback
тестового проекта на reference machine без xrun при buffer
по умолчанию, плюс start/stop 50 раз подряд без утечек
и зависаний.

**AUD-003.** Для каждого backend в UI отображается имя
backend'а, а не только имя устройства. Одно устройство
под разными backend'ами — разные записи.

**AUD-004.** Hot-plug: отключение текущего устройства во время
playback должно:
- остановить transport в течение 1 с;
- не вызвать crash или зависание UI;
- показать уведомление;
- не переключить вывод на другое устройство без действия
  пользователя (Amendment 01, раздел 5).

---

## 5. Audio settings: детализация

**AUD-010.** Структура capabilities (`audio-api`):

```
DeviceCapabilities {
  sample_rates: Discrete(Vec<u32>) | Range { min, max } | Unknown,
  buffer_size: Discrete(Vec<u32>) | Range { min, max, granularity }
             | FixedByDriver(u32) | Unknown,
  output_channels: u16,
  input_channels: u16,          // только отображение, запись вне scope
  sample_formats: Vec<SampleFormat>,
  reports_latency: bool,
  has_control_panel: bool,
  can_change_buffer_from_app: bool,
  can_change_sample_rate_from_app: bool,
}
```

**AUD-011.** Отображаемая «активная конфигурация» берётся из
фактически открытого stream, а не из запрошенных параметров:
- observed buffer size (min/max за последние N callbacks);
- фактический sample rate;
- reported latency в samples и ms;
- оценка round-trip latency с пометкой «reported by driver».

**AUD-012.** Хранение настроек: файл настроек пользователя
(не файл проекта), ключ = `(backend_id, device_persistent_id)`.
Если ОС не даёт стабильный device ID, используется имя
с пометкой «не стабильный ID».

**AUD-013.** Sample rate проекта и sample rate устройства могут
различаться. MVP: engine работает на sample rate устройства,
сэмплы ресемплируются при загрузке (BLD-041), плагины
переинициализируются при смене sample rate (PLG-040).
Ресемплинг выходного потока в callback не делается.

**AUD-014.** Смена устройства, sample rate или buffer size во время
playback: transport останавливается, stream перезапускается,
плагины переходят через `setActive(false)` →
`setupProcessing` → `setActive(true)`, transport остаётся
в остановленном состоянии.

---

## 6. ASIO-host backend: детализация

**ASIO-001.** Реализация в `crates/audio-asio`, собирается только
под `cfg(windows)` и feature `asio`. Сборка без feature работает
и не требует ASIO SDK.

**ASIO-002.** Лицензия ASIO SDK [VERIFY]: по имеющимся данным
SDK распространяется по выбору под проприетарной лицензией
Steinberg или под GPLv3. Выбор зависит от лицензии самого
приложения и решается в license review Milestone 0. Отдельно
проверить правила использования товарного знака и логотипа ASIO.

**ASIO-003.** Обнаружение драйверов: чтение
`HKLM\SOFTWARE\ASIO` (и 32-bit view не используется, так как
приложение 64-bit), CLSID каждого драйвера, проверка, что
COM-объект создаётся. Драйвер, который не создаётся,
показывается в списке как «недоступен» с кодом ошибки.

**ASIO-004.** Потоки:
- инициализация, `ASIOInit`, `ASIOCreateBuffers`, control panel,
  `ASIOExit` — на одном выделенном non-RT thread с COM STA
  (часть драйверов этого требует [VERIFY]);
- `bufferSwitch`/`bufferSwitchTimeInfo` — RT thread драйвера,
  действуют все правила раздела 8.

**ASIO-005.** Sample formats: поддержать минимум
`ASIOSTInt16LSB`, `ASIOSTInt24LSB`, `ASIOSTInt32LSB`,
`ASIOSTFloat32LSB`, `ASIOSTFloat64LSB`. Неподдерживаемый формат
даёт явную ошибку при открытии, а не тишину или шум.
Конвертация покрыта unit-тестами для каждого формата.

**ASIO-006.** Buffer size: использовать `ASIOGetBufferSize`
(min, max, preferred, granularity, где granularity = -1 означает
степени двойки). UI предлагает только допустимые значения.

**ASIO-007.** `asioMessage`:
- `kAsioResetRequest`: асинхронно, вне callback, полностью
  переоткрыть драйвер, сохранив выбранную конфигурацию;
- `kAsioBufferSizeChange`: то же, если драйвер не умеет
  менять размер на лету;
- `kAsioResyncRequest`, `kAsioLatenciesChanged`: обновить
  latency и diagnostics;
- `kAsioSupportsTimeInfo`: поддержать `bufferSwitchTimeInfo`.

**ASIO-008.** `ASIOOutputReady` вызывается, если драйвер
его поддерживает.

**ASIO-009.** Занятый драйвер (другое приложение держит устройство):
понятная ошибка, кнопка «повторить», без зависания UI
дольше 5 с (таймаут операций инициализации).

**ASIO-010.** Hardware acceptance для Audient (дополнение к
Amendment 01): матрица проверок:
- 44.1 и 48 kHz;
- минимальный стабильный buffer, 128, 256, 512;
- 30 минут playback тестового проекта с 2 VST3-плагинами
  на каждой комбинации, где это технически возможно;
- открытие control panel и смена buffer из неё во время
  работы приложения;
- выдёргивание USB во время playback;
- запуск при выключенном устройстве.

---

## 7. Собственный ASIO-драйвер: детализация

**DRV-001.** ADR-007 «Purpose of own ASIO driver» должен
содержать, помимо пунктов Amendment 01:
- список конкретных client-приложений, на которых проводится
  external-host acceptance (например, Reaper, Cubase/Nuendo,
  Bitwig, Ableton Live, FL Studio как *внешний клиент*
  для проверки совместимости — без анализа его внутренностей);
- целевую latency (в samples и ms) и способ измерения;
- 64-bit only или также 32-bit клиенты;
- модель поведения при нескольких клиентах одновременно.

**DRV-002.** Риск для MVP: Amendment 01 делает MVP зависимым
от deliverable, который ещё не определён. До ADR-007 этот
пункт фиксируется как top risk в `docs/risks.md`.
Владелец продукта решает в Milestone 0 одно из двух:
- оставить драйвер в критерии готовности MVP;
- вынести его в отдельный release track, не блокирующий MVP.

Решение оформляется как отдельный Amendment.

**DRV-003.** Если драйвер пишется на Rust, COM-объект
реализуется вручную или через bindings, и каждый
`extern "system"` entry point оборачивается в `catch_unwind`
с возвратом `ASE_HWMalfunction`/безопасного значения.

**DRV-004.** Регистрация: только per-machine в HKLM через
установщик с правами администратора; uninstaller удаляет
все записи и файлы; повторная установка идемпотентна.

---

## 8. Realtime constraints: детализация

### 8.1. Механизмы проверки

**RT-001.** В debug и test сборках audio callback оборачивается
в guard, который ловит аллокации и освобождения памяти
(собственный `#[global_allocator]` с thread-local флагом
«RT section» или аналог `assert_no_alloc` [VERIFY лицензию]).
Нарушение в тестах = fail.

**RT-002.** Guard не применяется к коду внутри вызова
стороннего плагина (раздел 11.6): плагины не контролируются
хостом, их нарушения измеряются, но не валят тесты хоста.
Граница guard'а проходит вокруг вызова `process()`.

**RT-003.** В release аллокации в callback не детектируются
(слишком дорого), но работают счётчики:
- время callback (p50/p99/max) относительно бюджета
  `buffer_size / sample_rate`;
- xrun/underrun counter;
- время, потраченное в каждом плагине.

Счётчики пишутся в атомики и читаются UI.

### 8.2. Реестр realtime-структур

**RT-010.** `docs/realtime-structures.md` содержит таблицу
для каждой структуры, которой касается audio thread:

| Структура | Направление | Crate | Alloc-free в RT | Lock-free | Wait-free | Ограничение времени |
|---|---|---|---|---|---|---|
| command queue UI→engine | SPSC | `rtrb` [VERIFY] | да | да | да | O(1) push/pop |
| snapshot swap | UI→RT | своя, AtomicPtr + возврат через queue | да | да | да | O(1) |
| meter/state feedback | RT→UI | triple buffer | да | да | да | O(1) |
| garbage return | RT→non-RT | SPSC | да | да | да | O(1) |
| plugin param changes | UI→RT | SPSC + pre-allocated IParameterChanges | да | да | да | O(N) по числу изменений за блок, N ограничен |

**RT-011.** Bounded-значения фиксируются константами и
проверяются тестами:
- максимум голосов на sampler-канал и глобально;
- максимум событий в блоке на инструмент;
- максимум изменений параметров плагина в блоке;
- максимальный поддерживаемый buffer size (`MAX_BLOCK`).

Переполнение очереди: событие отбрасывается, инкрементируется
счётчик, UI показывает предупреждение. Блокировки не происходит.

### 8.3. Размер буфера

**RT-020.** Engine обрабатывает callback любого размера от 1
до `MAX_BLOCK` сэмплов. Если driver прислал больше `MAX_BLOCK`,
callback делится на под-блоки ≤ `MAX_BLOCK`.

**RT-021.** Плагинам сообщается `maxSamplesPerBlock = MAX_BLOCK`
(или меньшее значение из настроек), и в `process()` никогда
не передаётся блок больше этого значения.

**RT-022.** Тест: offline render одного проекта с блоками
фиксированного размера 64 и со случайным размером из [1, 2048]
даёт идентичный результат для внутренних инструментов
(бит в бит или в пределах заданного допуска, если используется
float reassociation; допуск фиксируется).

### 8.4. Panic и аварии

**RT-030.** Профиль release: `panic = "unwind"`, чтобы работали
`catch_unwind` на FFI-границах. Решение (unwind против abort)
и его обоснование фиксируются в ADR.

**RT-031.** Все `extern "C"`/`extern "system"` callbacks,
которые вызывает внешний код (CPAL callback, ASIO callbacks,
VST3 host interfaces), оборачиваются в `catch_unwind`.
При panic: вывод тишины, атомарный флаг «engine faulted»,
UI предлагает перезапустить engine.

**RT-032.** Crash стороннего плагина (segfault, access violation)
при in-process хостинге не перехватывается надёжно и
завершает процесс. Поэтому работают autosave и recovery
(PER-020) и out-of-process scan (PLG-022).

### 8.5. Приоритет потоков

**RT-040.** Audio thread получает realtime-приоритет средствами ОС,
если backend этого не делает сам:
- Windows: MMCSS «Pro Audio»;
- macOS: audio workgroup / time-constraint policy [VERIFY,
  что даёт CPAL];
- Linux: SCHED_FIFO через rtkit или rlimits, с понятной
  диагностикой, если повысить приоритет не удалось.

---

## 9. CI и проверка: детализация

**CI-001.** CI matrix: `windows-latest`, `macos-14` (arm64),
`ubuntu-24.04`. Конкретные образы раннеров фиксируются;
обновление образа делается отдельным PR.

**CI-002.** Обязательные jobs на каждый PR:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- `cargo deny check`;
- проверка графа зависимостей (ARCH-001);
- headless offline render golden test;
- сборка Windows с feature `asio` (если SDK доступен в CI
  по лицензии, иначе job помечается и выполняется на self-hosted
  раннере);
- сборка VST3 host и тестовых плагинов;
- VST3 host integration tests на тестовых плагинах
  (раздел 11.10).

**CI-003.** Golden render: набор эталонных проектов
в `test-support/fixtures/`, render → WAV, сравнение с эталоном
по пиковой ошибке и RMS разности с заданным допуском.
Эталоны обновляются только явно, с объяснением в PR.

**CI-004.** GUI startup smoke test: приложение стартует
в CI (Linux под Xvfb, Windows и macOS на раннере), открывает
окно, загружает тестовый проект, закрывается с кодом 0
за ≤ 30 с.

**CI-005.** Nightly: release-сборки, пакеты, длительный
offline-stress (render 1 часа проекта), fuzzing парсера
проекта и WAV-загрузчика (`cargo fuzz`, только Linux).

**CI-006.** Пакетирование:
- Windows: MSI или Inno Setup [выбор в ADR] плюс portable zip;
- macOS: `.app` в `.dmg`, hardened runtime, entitlements
  из PLG-100;
- Linux: один формат, кандидат: AppImage или tar.gz
  [выбор в ADR]. Flatpak не выбирать для MVP: sandbox мешает
  доступу к системным путям VST3 и JACK [VERIFY].

---

## 10. Этапы: детализация и включение VST3

Изменения относительно Amendment 01 помечены `+`.

**Milestone 0:**
- Rust workspace;
- GUI window;
- CI на трёх ОС;
- лицензии, включая
  `+` license review ASIO SDK и VST3 SDK;
- architecture ADR;
- решение по назначению собственного ASIO-драйвера;
- `+` UI spike по BLD-020, включая встраивание окна плагина;
- `+` VST3 spike: загрузить SDK-пример плагина на трёх ОС,
  открыть его GUI в окне приложения, получить звук
  в offline render.

**Milestone 1:** без изменений.

**Milestone 1A, 1B:** без изменений.

**Последующие музыкальные milestone:** из исходной спецификации.

`+` **Milestone P1: VST3 effects** (после того, как в mixer
появились insert slots):
- scan (out-of-process), plugin browser;
- insert-эффекты в mixer;
- GUI плагина и generic editor;
- сохранение и восстановление state;
- missing-plugin placeholder;
- PDC в пределах PLG-050.

`+` **Milestone P2: VST3 instruments** (после того, как
Channel Rack и Piano Roll работают с внутренними инструментами):
- VST3-инструмент как канал Channel Rack;
- note on/off, velocity, note-off velocity;
- pitch bend и CC через parameter mapping (PLG-033);
- offline render с плагинами.

`+` **Milestone P3: plugin compatibility acceptance:**
прогон списка реальных плагинов (PLG-090) на трёх ОС.

MVP не считается завершённым без PASS по P1–P3
на всех трёх платформах.

---

## 11. Хостинг плагинов (новый раздел)

### 11.1. Scope MVP

**PLG-001.** В MVP входит:
- формат **VST3**;
- **инструменты** (VST3 Instrument) как источники звука
  в Channel Rack;
- **эффекты** (VST3 Fx) как insert-эффекты на каналах mixer
  и master;
- GUI плагина в отдельном окне;
- generic editor (список параметров) для любого плагина;
- сохранение и восстановление state плагина в проекте;
- presets: сохранение и загрузка state в файл `.vstpreset`
  [VERIFY: формат — часть SDK];
- offline render с плагинами;
- plugin delay compensation по PLG-050.

**PLG-002.** Вне scope MVP:
- **VST2**: SDK больше не распространяется Steinberg,
  новые лицензии не выдаются; не реализовывать и не
  использовать неофициальные заголовки;
- 32-bit плагины и bridge 32→64;
- x86_64-плагины на Apple Silicon (Rosetta bridge);
- Audio Units, LV2, LADSPA, AAX;
- CLAP (кандидат первого пост-MVP формата; архитектуру
  `plugin-api` не привязывать к VST3-специфике, см. PLG-010);
- sidechain inputs, multi-out инструменты (используется только
  main stereo out), surround;
- MIDI-эффекты и note expression;
- автоматизация параметров плагина в Playlist, если в исходной
  SPEC.md нет автоматизации внутренних параметров
  [OPEN QUESTION Q3];
- out-of-process хостинг для обработки звука (только scan).

### 11.2. Лицензирование и SDK

**PLG-005.** VST3 SDK [VERIFY]: по имеющимся данным начиная
с VST 3.8 SDK распространяется под лицензией MIT; ранее был
dual GPLv3/проприетарный. В Milestone 0 подтвердить по
закреплённой версии SDK:
- лицензию заголовков и `pluginterfaces`;
- лицензию hosting helpers (`public.sdk/source/vst/hosting`),
  если используются;
- правила использования логотипа и названия «VST»
  (VST Usage Guidelines / товарный знак Steinberg).

**PLG-006.** Rust bindings: выбрать один вариант в ADR:
- `vst3` crate (bindings, генерируемые из заголовков SDK)
  [VERIFY: лицензия, актуальность, поддержка трёх ОС];
- собственные bindings, генерируемые из заголовков
  закреплённой версии SDK.

Не использовать crate с лицензией, несовместимой с выбранной
лицензией приложения (например, GPLv3-bindings при
не-GPL приложении).

**PLG-007.** Логотип «VST» в UI и маркетинге используется
только после выполнения требований Steinberg.
До этого в UI пишется формат «VST3» как техническое название
без логотипа [VERIFY допустимость].

### 11.3. Архитектура

**PLG-010.** `plugin-api` определяет format-neutral интерфейсы:

```
PluginDescriptor   { id, format, name, vendor, version,
                     category, kind: Instrument|Effect,
                     path, arch, sdk_version }
PluginInstance     — lifecycle, activate/deactivate,
                     setup(sample_rate, max_block, precision)
PluginProcessor    — RT-часть: process(&mut ProcessData)
PluginController   — non-RT: параметры, state, editor
ParamInfo          { id, name, units, range, default,
                     steps, flags: automatable|readonly|bypass|... }
PluginEditor       — attach(RawWindowHandle), size, resize, close
```

VST3-специфичные типы не выходят за пределы `plugin-vst3`.

**PLG-011.** Разделение на RT- и non-RT части обязательно:
`engine` держит только `PluginProcessor`, UI — только
`PluginController`/`PluginEditor`. Связь между ними идёт через
очереди из раздела 8.

**PLG-012.** Модель процессов MVP:
- scan: **out-of-process**, отдельный бинарник `plugin-scanner`;
- загрузка и обработка: **in-process**.

Out-of-process обработка (sandbox per plugin) рассматривается
после MVP через ADR. Причины отложить: сложность встраивания
GUI между процессами (особенно на macOS) и IPC в RT-пути.

### 11.4. Обнаружение и сканирование

**PLG-020.** Стандартные пути VST3 [VERIFY по документации SDK]:
- Windows: `%COMMONPROGRAMFILES%\VST3`
  (`C:\Program Files\Common Files\VST3`);
- macOS: `/Library/Audio/Plug-Ins/VST3`,
  `~/Library/Audio/Plug-Ins/VST3`;
- Linux: `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3`.

Пользователь может добавить свои пути. Пути хранятся в
пользовательских настройках, а не в проекте.

**PLG-021.** Поддерживаемая форма плагина:
- bundle `.vst3` с бинарником для текущей архитектуры
  (`Contents/x86_64-win/`, `Contents/MacOS/`,
  `Contents/x86_64-linux/`);
- на Windows дополнительно legacy single-file `.vst3` DLL.

Бинарник не той архитектуры не загружается; плагин
показывается как «incompatible architecture».

**PLG-022.** Алгоритм scan:
1. найти кандидатов в путях;
2. если есть `Contents/Resources/moduleinfo.json`, прочитать
   метаданные без загрузки бинарника [VERIFY: версия SDK
   и надёжность];
3. иначе запустить `plugin-scanner <path>` как отдельный
   процесс, который загружает модуль, читает factory
   и `PClassInfo2`, и возвращает JSON в stdout;
4. таймаут scanner'а — 30 с на модуль (настраивается);
5. crash, таймаут или неверный вывод → плагин попадает
   в blocklist с причиной;
6. результат кешируется: ключ `(path, size, mtime, arch)`.

**PLG-023.** Scan никогда не блокирует UI и не запускается
автоматически на каждом старте полностью: на старте
проверяются только изменения по кешу. Полный rescan
запускает пользователь.

**PLG-024.** Blocklist виден пользователю; плагин можно
вручную вернуть и пересканировать.

**PLG-025.** Плагин, который загрузился при scan, но упал при
создании экземпляра в проекте, помечается и при следующем
открытии проекта не загружается автоматически: пользователь
получает запрос «загрузить снова?» (защита от crash-loop при
открытии проекта).

### 11.5. Lifecycle экземпляра

**PLG-030.** Последовательность (VST3), все шаги кроме `process`
выполняются вне audio thread:
1. загрузка модуля, `InitDll`/`bundleEntry`/`ModuleEntry`
   по платформе;
2. `IPluginFactory::createInstance` → `IComponent`;
3. `IComponent::initialize(hostContext)`;
4. получение `IEditController` (тот же объект или отдельный
   через `getControllerClassId`), `initialize`;
5. соединение component ↔ controller через `IConnectionPoint`,
   если реализовано;
6. `setComponentState` у controller'а после `setState`
   у component;
7. настройка bus arrangement (PLG-031);
8. `IAudioProcessor::setupProcessing(sampleRate, maxBlock,
   kSample32, kRealtime|kOffline)`;
9. `IComponent::setActive(true)`;
10. `IAudioProcessor::setProcessing(true)` — разрешено
    вызывать в audio thread по спецификации VST3 [VERIFY];
11. `process()` на audio thread;
12. завершение в обратном порядке, `terminate`, release,
    выгрузка модуля только после освобождения всех
    экземпляров этого модуля.

**PLG-031.** Bus arrangement MVP:
- эффект: 1 stereo in → 1 stereo out; mono-эффекты
  поддерживаются через up/downmix в хосте;
- инструмент: 1 event in, main stereo out; остальные выходы
  деактивируются (`activateBus(false)`);
- если плагин не принимает stereo, хост пробует mono
  и делает адаптацию; если ничего не подходит — понятная
  ошибка.

**PLG-032.** Precision: MVP использует `kSample32`.
`kSample64` не используется даже при поддержке плагином.

**PLG-033.** События для инструментов:
- note on/off с `noteId`, velocity в [0, 1],
  sample offset внутри блока (sample-accurate);
- pitch bend, mod wheel, sustain и прочие CC — через
  `IMidiMapping` → parameter changes, если плагин его реализует;
- при остановке transport и при seek: note off всем активным
  нотам этого инструмента;
- «panic» (all notes off) доступен из UI.

**PLG-034.** Transport/`ProcessContext` заполняется в каждом
блоке: tempo, time signature, `projectTimeSamples`,
`projectTimeMusic` (PPQ), `barPositionMusic`, флаги
`kPlaying`, `kCycleActive` (если loop), `kTempoValid`,
`kTimeSigValid`, `kProjectTimeMusicValid`. Loop/cycle
корректно отражается при переходе через границу цикла.

**PLG-035.** `IComponentHandler::restartComponent` обрабатывается
вне audio thread:
- `kLatencyChanged` → пересчёт PDC (PLG-050);
- `kParamValuesChanged`, `kParamTitlesChanged` → обновить UI;
- `kIoChanged`, `kReloadComponent` → deactivate → reconfigure
  → activate с остановкой transport этого плагина.

**PLG-036.** Host-интерфейсы, которые предоставляет приложение:
`IHostApplication`, `IComponentHandler` (`beginEdit`,
`performEdit`, `endEdit`, `restartComponent`), `IPlugFrame`
(resize), `IAttributeList`/`IMessage` для `IConnectionPoint`,
`IParameterChanges`/`IParamValueQueue` (pre-allocated),
`IEventList` (pre-allocated), на Linux — `IRunLoop`
(PLG-062). Остальные (`IComponentHandler2`, `IUnitHandler`,
`IProgress`, `IContextMenu`) — опционально; отсутствие
не должно ломать плагины, которые проверяют их через
`queryInterface`.

### 11.6. Realtime и плагины

**PLG-040.** Хост гарантирует для своего кода правила раздела 8.
Хост **не может** гарантировать их для кода плагина: плагин
может аллоцировать, брать locks или работать дольше бюджета.
Хост измеряет время каждого плагина и показывает его в UI
(per-plugin CPU), но не прерывает плагин.

**PLG-041.** Все буферы `ProcessData` (audio buffers, event list,
parameter changes, output parameter changes) выделяются
при `setupProcessing` на `MAX_BLOCK` и не меняют размер
в audio thread.

**PLG-042.** Параметры:
- UI → processor: изменения из generic editor и из собственного
  GUI плагина (`performEdit`) идут через SPSC-очередь
  в audio thread и попадают в `inputParameterChanges`
  следующего блока с sample offset 0;
- processor → UI: `outputParameterChanges` копируются в
  RT→UI очередь; controller обновляется в UI thread;
- переполнение очереди обрабатывается по RT-011.

**PLG-043.** Bypass: если у плагина есть параметр с `kIsBypass`,
используется он; иначе хост делает bypass сам
(копирование входа в выход с учётом latency, чтобы
не было скачка при переключении).

**PLG-044.** Смена sample rate или max block size требует
deactivate → `setupProcessing` → activate для всех плагинов
и выполняется только при остановленном stream (AUD-014).

**PLG-045.** Offline render: `processMode = kOffline`; плагинам
передаётся ровно та же последовательность событий, что при
realtime playback. Бит-в-бит идентичность realtime и offline
для плагинов **не** требуется и не обещается: многие плагины
недетерминированы. Требуется только отсутствие пропусков
и корректный тайминг событий.

### 11.7. Latency и PDC

**PLG-050.** MVP поддерживает компенсацию задержки
(`getLatencySamples`) для топологии:
- channel insert chain → mixer track insert chain → master;
- общая задержка выравнивается по максимальной задержке среди
  параллельных путей к master;
- инструменты VST3 учитываются так же.

Sends, sidechain и произвольный routing между треками
(если они есть в исходной спецификации) — PDC для них
вне MVP [OPEN QUESTION Q4].

**PLG-051.** Изменение latency плагина (`kLatencyChanged`)
пересчитывает PDC вне audio thread; новая конфигурация
применяется через swap snapshot (ARCH-004). Допустим
кратковременный щелчок; crash и рассинхрон недопустимы.

**PLG-052.** Суммарная latency плагинов отображается в UI
mixer'а по каждому треку и в master.

### 11.8. GUI плагина

**PLG-060.** Окно редактора — отдельное top-level native окно
приложения (не встроенное в egui-канвас), владелец — главное
окно. Тип view [VERIFY]:
- Windows: `kPlatformTypeHWND`;
- macOS: `kPlatformTypeNSView`;
- Linux: `kPlatformTypeX11EmbedWindowID`.

Перед `attached()` хост проверяет `isPlatformTypeSupported`.

**PLG-061.** Размер:
- начальный размер из `IPlugView::getSize`;
- `IPlugFrame::resizeView` от плагина обрабатывается;
- пользовательский resize — только если `canResize()`,
  с `checkSizeConstraint`;
- HiDPI на Windows и Linux: `IPlugViewContentScaleSupport::
  setContentScaleFactor`, если плагин поддерживает
  [VERIFY]; на macOS масштаб берётся из backing scale
  NSView.

**PLG-062.** Linux:
- VST3 GUI на Linux работает через X11; хост обязан
  предоставить `Steinberg::Linux::IRunLoop` (регистрация
  file descriptors и таймеров плагина) и обслуживать его
  в UI event loop;
- под Wayland окна редакторов создаются через XWayland.
  Варианты: всё приложение на X11 backend winit или только
  окна плагинов на X11 — решается в ADR по результатам
  UI spike;
- без X11/XWayland плагины работают, но GUI недоступен —
  показывается generic editor с объяснением.

**PLG-063.** Закрытие окна редактора не уничтожает экземпляр
плагина. Удаление плагина из проекта сначала закрывает
редактор (`removed()`), затем освобождает экземпляр.

**PLG-064.** Generic editor обязателен: список параметров
с именем, значением в единицах плагина (`getParamStringByValue`)
и слайдером; ввод текстом (`getParamValueByString`).
Работает для плагинов без собственного GUI
(`createView` вернул null).

**PLG-065.** Вызовы `IEditController` и `IPlugView` — только
в UI thread. Callbacks плагина в хост (`performEdit` и т.д.),
пришедшие не из UI thread, должны быть безопасны
(очередь, без паники).

### 11.9. Persistence плагинов

**PLG-070.** В проекте для каждого экземпляра хранится:

```json
{
  "plugin_ref": {
    "format": "vst3",
    "class_id": "<16 байт hex, FUID>",
    "name": "...",
    "vendor": "...",
    "version": "...",
    "category": "Instrument|Fx|..."
  },
  "component_state": "<base64 или ссылка на asset-файл>",
  "controller_state": "<base64 или ссылка на asset-файл>",
  "bypass": false,
  "editor_window": { "open": false, "x": 0, "y": 0 }
}
```

Поиск плагина при загрузке — по `class_id`, а не по пути
к файлу. Абсолютные пути к плагинам в проект не пишутся.

**PLG-071.** Состояние больше порога (например, 256 KiB)
выносится в отдельный файл в asset-папке проекта и
подпадает под safe save (атомарная запись, recovery).

**PLG-072.** Отсутствующий плагин при открытии проекта:
- создаётся placeholder с именем, vendor и class_id;
- placeholder пропускает звук без обработки (эффект)
  или молчит (инструмент);
- state хранится как есть и записывается обратно при сохранении
  без изменений (round-trip без потерь);
- пользователь видит список отсутствующих плагинов при
  открытии проекта.

**PLG-073.** Версия плагина при загрузке отличается от сохранённой:
state передаётся плагину как есть (VST3 отвечает за
совместимость state), пользователю показывается
информационное уведомление.

**PLG-074.** Тест round-trip: save → load → save для проекта
с плагинами даёт идентичные state-блобы, если плагин
детерминированно сериализует state; для placeholder —
всегда идентичные.

**PLG-075.** Autosave во время работы с плагинами (см. RT-032):
интервал по умолчанию 5 минут, плюс сохранение перед
открытием GUI плагина, помеченного как «падал раньше».
Получение state плагина для autosave выполняется в UI thread
и не блокирует audio thread.

### 11.10. Тестирование плагинов

**PLG-080.** Тестовые плагины в `test-plugins/` собираются в CI
на трёх ОС:
- пример(ы) из VST3 SDK (например, `again` — эффект,
  `note_expression_synth` или аналогичный — инструмент)
  [VERIFY: что есть в закреплённой версии SDK и под какой
  лицензией примеры];
- собственный fault-injection плагин с режимами:
  - crash в factory (проверка blocklist при scan);
  - зависание в factory (таймаут scanner'а);
  - заявленная latency N сэмплов (проверка PDC);
  - смена latency на лету (`restartComponent`);
  - выдача `outputParameterChanges`;
  - медленный `process()` (превышение бюджета, счётчики);
  - mono-only bus arrangement;
  - отсутствие GUI;
  - resize GUI с ограничениями.

**PLG-081.** Integration-тесты хоста (headless, без GUI):
- scan тестовых путей, корректный кеш и blocklist;
- загрузка, setup, process, unload без утечек
  (повтор 1000 раз);
- offline render с эффектом и инструментом против
  golden-файла (для детерминированных тестовых плагинов);
- PDC: импульс через плагин с latency N выравнивается
  с параллельным путём без плагина с точностью до сэмпла;
- state round-trip (PLG-074);
- missing plugin placeholder.

**PLG-082.** Ручной GUI-тест (чеклист в `docs/acceptance/`):
открыть, двигать регулятор в GUI плагина и видеть изменение
в generic editor, resize, закрыть, повторно открыть,
HiDPI, закрыть проект с открытыми окнами.

### 11.11. Совместимость с реальными плагинами

**PLG-090.** Compatibility list для Milestone P3 — минимум
6 плагинов, из них минимум 3 кросс-платформенных с VST3
на всех трёх ОС. Кандидаты (проверить наличие VST3-сборок
под Apple Silicon и Linux на момент теста) [VERIFY]:
- Surge XT (инструмент, Win/macOS/Linux);
- Dexed (инструмент, Win/macOS/Linux);
- Vital (инструмент);
- Airwindows Consolidated (эффекты);
- один коммерческий эффект с тяжёлым GUI;
- один плагин с ненулевой latency (лимитер/linear-phase EQ).

**PLG-091.** Для каждого плагина в отчёт записываются:
версия, ОС, архитектура, scan, load, звук, GUI, resize,
HiDPI, state round-trip, offline render, PDC (если есть
latency), стабильность за 30 мин playback, найденные
проблемы. Без проверки — NOT RUN.

**PLG-092.** Проблемы совместимости, вызванные плагином
(подтверждено в другом хосте), документируются как known
issues и не блокируют MVP. Проблемы хоста блокируют.

**PLG-093.** Коммерческие плагины не поставляются с приложением
и не используются в CI, если их лицензия этого не разрешает.

### 11.12. Платформенные особенности

**PLG-100.** macOS:
- hardened runtime + notarization требуют entitlement
  `com.apple.security.cs.disable-library-validation`, иначе
  плагины других разработчиков не загрузятся [VERIFY];
- проверить необходимость
  `com.apple.security.cs.allow-unsigned-executable-memory`
  и `allow-jit` для отдельных плагинов; включать только
  при подтверждённой необходимости;
- плагины с quarantine-атрибутом: показать понятную ошибку
  Gatekeeper, не снимать quarantine автоматически;
- только arm64 или universal-плагины (PLG-002).

**PLG-101.** Windows:
- `LoadLibraryExW` с безопасными флагами поиска DLL
  (`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR` и т.п.) [VERIFY,
  совместимость с плагинами, которые грузят соседние DLL];
- COM: UI thread инициализирован как STA, если это требуется
  плагинами для GUI [VERIFY];
- DPI awareness приложения — Per-Monitor V2; поведение
  DPI-unaware плагинов документируется.

**PLG-102.** Linux:
- `dlopen` с `RTLD_LOCAL`;
- зависимость от X11 (PLG-062);
- проверить конфликты символов (например, разные версии
  libstdc++/GTK внутри плагинов) на reference distribution.

---

## 12. Persistence: дополнения

**PER-001.** Формат проекта: каталог или один файл
[решение в ADR]. В корне — `project.json` с полем
`format_version` (целое число). Любое изменение схемы
увеличивает версию и сопровождается миграцией и тестом.

**PER-002.** Safe save: запись во временный файл в том же
каталоге → fsync → атомарный rename → fsync каталога
(где ОС это поддерживает). Предыдущая версия сохраняется
как `.bak`.

**PER-003.** Ссылки на сэмплы: относительные пути внутри
проекта; внешние файлы — опция «собрать в проект»
(collect assets). Отсутствующий сэмпл не мешает открытию
проекта.

**PER-010.** Проект с плагинами — см. PLG-070…PLG-075.

**PER-020.** Recovery: autosave в отдельный каталог; при старте
после аварийного завершения пользователь получает предложение
восстановить проект. Признак аварии — lock-файл сессии.

**PER-030.** Fuzzing парсера проекта (CI-005); повреждённый
файл не вызывает crash и показывает понятную ошибку.

---

## 13. Clean-room и provenance

**CR-001.** Проект — независимая реализация DAW с похожим
workflow (pattern-based sequencing, channel rack, piano roll,
playlist, mixer). Цель — функциональное сходство на уровне
общеизвестных концепций, а не копия продукта.

**CR-002.** Запрещено:
- декомпиляция, дизассемблирование, анализ трафика или памяти
  FL Studio и его плагинов;
- использование утёкших исходников, внутренних документов,
  чужих ресурсов (иконки, скины, звуки, пресеты, демо-проекты);
- копирование trade dress: точная раскладка, цвета, иконки,
  названия фирменных элементов;
- использование названий «FL Studio», «Image-Line» и названий
  их плагинов в продукте, кроме описательных ссылок
  в документации, если юрист это одобрит;
- импорт/экспорт `.flp` в MVP.

**CR-003.** Разрешено: общедоступная пользовательская документация
и общие знания о DAW как источник требований к *поведению*.
Все такие источники перечисляются в `docs/provenance/sources.md`.

**CR-004.** Для UI используются собственные названия элементов.
Таблица соответствия «наше название ↔ общий термин» хранится
в `docs/glossary.md`.

**CR-005.** AI-assisted разработка:
- в контекст AI-агентов не передаются материалы из CR-002;
- AI-сгенерированный код проходит тот же review, что
  человеческий, включая проверку лицензий заимствованных
  фрагментов;
- код, явно воспроизводящий известную стороннюю реализацию
  (например, фрагменты JUCE или VST3 SDK вне разрешённой
  лицензии), не принимается;
- в PR указывается, какие части сгенерированы AI
  (политика маркировки — в `CONTRIBUTING.md`).

**CR-006.** Тестовые сэмплы и пресеты — собственные или под
лицензией CC0/совместимой, с записью источника.

---

## 14. Список ADR

| ADR | Тема | Milestone |
|---|---|---|
| ADR-001 | Лицензия приложения | 0 |
| ADR-002 | UI-стек (по BLD-020) | 0 |
| ADR-003 | Модель потоков и RT-структуры | 0 |
| ADR-004 | Формат проекта и миграции | 0 |
| ADR-005 | Panic strategy (RT-030) | 0 |
| ADR-006 | ASIO-host: CPAL против собственного адаптера | 0 |
| ADR-007 | Назначение собственного ASIO-драйвера | 0 |
| ADR-008 | VST3 bindings (PLG-006) | 0 |
| ADR-009 | Модель процессов плагинов (PLG-012) | 0 |
| ADR-010 | Linux: X11/Wayland для окон плагинов (PLG-062) | 0 |
| ADR-011 | Формат пакетов на каждой ОС (CI-006) | до release |
| ADR-012 | Ресемплинг (BLD-041) | 1 |

ADR хранится в `docs/adr/NNN-title.md`: контекст, варианты,
решение, последствия, статус.

---

## 15. Риски

| Риск | Вероятность | Влияние | Митигация |
|---|---|---|---|
| Собственный ASIO-драйвер не определён, но блокирует MVP | высокая | высокое | DRV-002 |
| Встраивание GUI VST3 в egui/winit на трёх ОС | средняя | высокое | UI spike M0, BLD-020 |
| Crash плагина роняет DAW | высокая | среднее | out-of-process scan, autosave, crash-loop защита |
| Linux GUI плагинов под Wayland | высокая | среднее | XWayland, generic editor |
| macOS hardened runtime блокирует плагины | средняя | высокое | PLG-100, проверка на notarized сборке |
| CPAL не закрывает ASIO/JACK требования | высокая | среднее | BLD-030, собственные адаптеры |
| Лицензии SDK несовместимы с лицензией приложения | низкая/средняя | высокое | license review M0 |
| Плагины нарушают RT-правила | высокая | низкое | измерение per-plugin CPU, PLG-040 |

---

## 16. Открытые вопросы

- **Q1.** Лицензия самого приложения (проприетарная, GPL,
  MIT/Apache)? От неё зависит выбор лицензии ASIO SDK
  и допустимых bindings.
- **Q2.** Оставить собственный ASIO-драйвер в критерии
  готовности MVP или вынести в отдельный track (DRV-002)?
- **Q3.** Есть ли в исходной SPEC.md автоматизация параметров?
  Если да, параметры VST3 должны стать automation targets в MVP.
- **Q4.** Какой routing mixer'а в исходной SPEC.md (sends,
  sidechain)? Это определяет объём PDC.
- **Q5.** MIDI input с внешней клавиатуры: входит ли в MVP?
  Если да, нужен отдельный раздел (CoreMIDI, WinMM/WinRT
  MIDI, ALSA seq) и routing MIDI → VST3-инструмент.
- **Q6.** Нужен ли CLAP в MVP наравне с VST3, учитывая, что
  его лицензия (MIT) и Rust-экосистема проще?
- **Q7.** Нужны ли Audio Units на macOS для MVP? Сейчас — нет.
