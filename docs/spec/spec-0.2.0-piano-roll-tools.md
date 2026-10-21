# Дополнение к спецификации 0.2.0 — Piano Roll Tools

Этот документ добавляет раздел 18 к ранее подготовленной спецификации 0.2.0. Остальные требования сохраняются.


## 18. Piano Roll Tools — расширение спецификации

### 18.1. Назначение и clean-room границы

Добавить в Piano Roll меню Tools с инструментами, представленными на пользовательском скриншоте. Названия используются как функциональные ориентиры. Ниже определено собственное поведение приложения, а не обещание точной совместимости с алгоритмами FL Studio.

Не копировать исходники, скрипты, MIDI-пресеты, chord maps, ресурсы и оформление оригинала. Все алгоритмы, presets и тестовые fixtures создать самостоятельно.

Раздел включает Built-in Tools и Script Tools. Все функции обязательны для завершения этого расширения; допускается последовательная реализация. Не оставлять активных пунктов с фиктивным действием.

### 18.2. Общий контракт обработки

- Рабочий контекст: текущий Pattern и текущий InstrumentChannel, не весь проект и не ghost notes.
- Если выбраны ноты — обрабатывать только их; иначе все ноты текущего канала. Генераторы используют отдельные правила диапазона ниже.
- Генераторы: выбранный time range; при его отсутствии диапазон текущего Pattern.
- Пустой вход у преобразований — no-op, не автоматическая генерация.
- Все позиции и длительности целочисленные, PPQ = 960; минимальная длительность 1 tick, pitch 0..127, velocity 1..127.
- Новые ноты получают новые ID; изменённые сохраняют ID. При split первая часть сохраняет исходный ID, остальные получают новые.
- Существующие ноты вне scope не менять.
- После Apply выделять изменённые/созданные ноты. Для selection-only tools сохранять результат selection.
- Диалоговые tools: Preview, Apply, Cancel, Reset. Preview строится от исходного snapshot, не от предыдущего preview.
- Apply — одна undo command; Cancel полностью восстанавливает исходное состояние. Selection-only operations также восстанавливаются через editor selection history.
- Quick-команды применяются сразу как одна undo command.
- Preview не изменяет project dirty state до Apply. При закрытии окна — Cancel.
- Повторные Apply считаются отдельными пользовательскими операциями.
- Изменения вычислять вне audio callback и публиковать через существующий playback snapshot mechanism.
- Общий лимит результата одной операции: 100 000 нот. Превышение отклонять до изменения проекта, без частичного применения.
- Добавить progress/cancel для тяжёлых генераторов.
- Параметры каждого tool имеют versioned schema. Последние настройки — user settings; результат — обычные project notes.
- Random tools используют фиксируемый u64 seed, закреплённый PRNG algorithm/version и стабильный порядок входа: start, pitch, ID. Одинаковые input/settings/seed дают одинаковый результат.
- Shortcut не копировать слепо: проверить конфликты, текстовый ввод и platform-specific modifiers.

#### Границы и округление

Нота всегда находится внутри Pattern. Общее правило: сначала вычислить candidate в signed arithmetic, затем проверить границы. Длительность обрезать по Pattern end. Если после преобразования start оказался вне Pattern или duration < 1, не удалять ноту молча: показать количество затронутых нот и потребовать подтверждение исключения либо отмену. Исключения с другим поведением описаны явно.

Snap rounding: nearest, ties toward later position. Snap Off у quick grid tools использует последнюю явную grid division, default 1/16; показывать используемую сетку в tooltip.

Для pitch overflow — clamp 0..127, с указанием числа ограниченных нот в preview. Pitch collisions не склеивать автоматически.

### 18.3. Riff Machine — генератор музыкальной фразы

Собственный staged generator. Не использовать оригинальные preset scores.

Параметры:
- key root, scale: major/natural minor;
- range start/length, pitch min/max;
- rhythm division, density;
- chord progression или fixed chord;
- arp mode, octave range;
- gate;
- velocity base/variation;
- timing humanization;
- seed;
- insertion mode: Replace range / Append layer.

Pipeline: rhythm → harmony → voicing → arpeggiation → gate → velocity/timing variation → boundary validation.

Replace range удаляет только ноты текущего канала, чьи start находятся в range; пересекающиеся ноты с началом до range сохраняются. Append layer сохраняет существующие ноты. Default — Replace range с видимым preview удалений.

Тест: воспроизводимость, соблюдение pitch/scale/range, no mutation after Cancel.

### 18.4. Generate Chord Progression

Создаёт последовательность аккордов в выбранном диапазоне.

Параметры: tonic, major/natural minor, degrees I..VII, chord type triad/seventh, chord duration, octave, inversion, close voicing, insertion mode.

Ступени и качество аккордов вычислять по выбранной гамме. Seventh строить по диатоническим терциям. Возможность редактировать последовательность до Apply.

Voice leading: детерминированно выбрать допустимые обращения с минимальной суммой перемещений голосов; при равенстве — лексикографически меньший набор pitches. Для разных количеств голосов не заявлять оптимальный voice leading: применить documented fallback.

Последний аккорд обрезать по range end. Replace/Append semantics как у Riff Machine.

### 18.5. Quick Legato

Группировать ноты с одинаковым start в onset groups. Конец всех нот группы перенести на начало следующей onset group в scope. Это global-onset legato, не распознавание мелодических голосов.

Последняя группа сохраняет исходные durations. Ноты вне selection не использовать как скрытую границу. Pitch, velocity и starts не менять.

### 18.6. Articulate

Диалог управления длительностью:
- mode: Gate / Legato;
- gate percent 1..200%;
- gap ticks >= 0;
- duration variation 0..100%;
- seed;
- prevent overlap with next onset on/off.

Gate: исходная duration × gate factor. Legato: расстояние до следующей onset group; последняя группа использует исходную duration. Затем variation и gap; итог не менее 1 tick. Prevent overlap ограничивает результат следующей onset group.

Это изменение MIDI-note timing, не управление VST articulation keyswitches.

### 18.7. Quick Quantize

Привязать start и end ноты к текущей сетке независимо. Если end <= start, установить duration = 1 tick. Velocity и pitch не менять.

### 18.8. Quick Quantize Start Times

Привязать только start; сохранить duration, кроме необходимого ограничения Pattern end.

### 18.9. Quantize

Параметры:
- grid division, включая triplets;
- strength 0..100%;
- targets: start / end;
- swing 0..75% от grid step;
- window ticks: максимальное расстояние захвата;
- preserve duration при start-only;
- optional custom groove template.

Swing сдвигает нечётные grid points вправо; индексация начинается с Pattern tick 0. Strength интерполирует исходную позицию к ближайшей groove/grid point с итоговым округлением до tick.

При start+end duration вычисляется из новых границ. Нулевые/отрицательные lengths исправляются до 1 tick. Custom groove — собственный JSON schema, не proprietary preset import.

### 18.10. Quick Chop

Разделить каждую ноту на последовательные отрезки длиной текущего grid step, начиная от исходного start. Остаток — отдельная короткая нота. Сохранить pitch и velocity. Не добавлять промежутков и не смещать другие ноты.

### 18.11. Chop

Параметры: fixed step / N equal parts, gate 1..100%, alternating velocity multiplier, reverse velocity pattern.

Equal parts: границы вычислять от абсолютного исходного диапазона, не накоплением округлённого step. N не больше duration в ticks. При gate < 100% сокращать каждый сегмент от его начала, минимум 1 tick.

### 18.12. Glue

Объединять соприкасающиеся или перекрывающиеся ноты одного pitch в scope. Group by pitch и будущие note-routing attributes; ноты разных направлений не объединять.

Параметры: gap tolerance ticks, default 0.

Результат: min start..max end, velocity первой ноты по порядку start/ID, ID первой ноты. Несовпадение velocity показать в preview. При tolerance > 0 заполняется и промежуток.

### 18.13. Arpeggiate

Преобразует onset groups аккордов в последовательность нот.

Параметры: up/down/up-down/down-up/random, step, gate, octave span 1..4, seed, scope range.

Каждая onset group действует до следующей onset group либо максимального исходного end последней группы. Не генерировать за пределами активного диапазона.

Pitch pool: уникальные pitches группы, расширенные по октавам в допустимом MIDI range. Up-down не дублирует крайние pitches на повороте. Velocity берётся от соответствующей исходной ноты. Исходные notes заменяются результатом.

### 18.14. Strum

Сдвигает ноты аккорда последовательными задержками.

Параметры: direction low-to-high/high-to-low, delay ticks, onset grouping tolerance ticks, velocity ramp, preserve ends on/off.

Группы формируются относительно первого start группы, не цепочкой соседних расстояний. Сдвиг i-й ноты = i × delay.

Preserve ends off: сохранить duration. On: сохранить исходный end; варианты с end <= new start пометить invalid и требовать изменения параметров. Не удалять их молча.

### 18.15. Flam

Добавляет короткую grace note перед каждой исходной нотой.

Параметры: offset ticks, grace duration ticks, grace velocity factor, negative-boundary policy: Skip grace / Shift pair.

Default: Skip grace, если grace start < 0. Shift pair сдвигает основную ноту настолько, чтобы grace start = 0; изменение явно показано в preview. Grace end не позже main start. Основная нота сохраняет ID.

### 18.16. Claw Machine — rhythmic mask

Самостоятельный rhythmic slicer, не точная копия оригинального алгоритма.

Параметры: period ticks, slices per period, remove every Nth slice, phase, mode Remove / Gate / Shift, shift ticks.

Сначала пересечь note intervals с границами slices. Нумерация slices — от range start, phase применяется по модулю количества slices.

- Remove: исключить фрагменты в отмеченных slices.
- Gate: сохранить фрагменты только в отмеченных slices.
- Shift: сдвинуть фрагменты отмеченных slices на shift ticks.

Pitch/velocity сохраняются. Пустые фрагменты не создавать. До Apply показать число split/deleted notes. Collisions после shift не склеивать.

### 18.17. Limit

Ограничивает диапазон высоты и при необходимости приводит к гамме.

Параметры: pitch min/max, transpose semitones, mode Clamp / Octave fold, optional scale root/type.

Порядок: transpose → optional scale snap → range limiting.

Scale snap выбирает ближайшую разрешённую ступень; ties — вниз. Octave fold выбирает pitch того же pitch class в диапазоне с минимальным смещением; если такого нет, reject с объяснением, не скрытый clamp.

Timing/velocity не меняются.

### 18.18. Flip

- Horizontal: start' = range_start + range_end - original_end; duration сохраняется.
- Vertical: pitch' = 2 × pivot_pitch - pitch.

Параметры: horizontal/vertical независимо, pivot pitch. Default pivot — округлённый вниз центр min/max pitches. Default range — min start..max end scope.

Velocity сохраняется. Pitch overflow показывается в preview.

### 18.19. Randomize

Режимы: Transform existing / Generate.

Transform: независимые pitch, start, duration, velocity ranges; scale constraint; seed. Default deviations = 0, preview не меняет данные до выбора параметров.

Generate: time range, grid, density 0..1, pitch range, major/natural minor/chromatic, polyphony cap, duration range, velocity range, seed, Replace/Append.

Не смешивать probability density и количество обязательных нот: density задаёт вероятность события в cell. Density = 0 создаёт ноль нот; density = 1 активирует все допустимые cells до применения polyphony cap.

### 18.20. Scale Levels

Target в текущем note model: velocity. Не добавлять неработающие pan/release/filter targets.

Параметры: center 1..127, multiplier, offset, invert, curve Linear / Power, exponent > 0.

Нормализовать velocity как x=(velocity-1)/126. Применить affine transform вокруг normalized center, clamp 0..1, optional inversion x=1-x, затем x^exponent. Вернуть round(1+126*x).

Default multiplier=1, offset=0, invert=false, exponent=1 — identity.

Расширять target list только при реализации соответствующих note attributes и их DSP playback.

### 18.21. LFO

Создаёт curve data для существующего AutomationSource, не аудио-осциллятор.

Не писать continuous LFO в MIDI-note velocity без отдельного явно выбранного режима.

Если открыт только Piano Roll:
- предложить выбрать поддерживаемый automation target;
- создать AutomationSource по подтверждению;
- открыть Automation Editor;
- автоматически не размещать clip в Playlist.

Параметры: sine/triangle/square, phase, normalized center/amplitude, period ticks, start/end values для center/amplitude/period, resolution ticks.

Range: выделенный time range, иначе один такт от Pattern start. Существующие точки внутри range заменяются после подтверждения, вне range сохраняются.

Square кодируется step segments; sine/triangle — linear segments с заданным разрешением. Для переменного периода phase определяется интегрированием frequency, не подстановкой instantaneous period в абсолютную фазу.

Ограничить число points; preview показывает итоговый объём. Apply — одна undo command.

### 18.22. Script Tools и команды Scripts

В меню отобразить:
- Run last script again;
- Open script folder;
- Arpeggiator;
- Euclidean;
- Humanize;
- Note repeater sprinkler;
- Select by velocity and more.

Built-in Arpeggiate и script Arpeggiator могут использовать общий чистый algorithm service. Не создавать два расходящихся DSP/note-processing алгоритма.

#### Модель исполнения

Для 0.2.0 выбрать ограниченный embedded script runtime Rhai после проверки версии/лицензии. Это собственный API, не совместимый с Python API FL Studio.

Пользовательские scripts — plain text, без загрузки native modules. File/network/process access не предоставлять. Ограничить operations, recursion/call depth, memory-related collection sizes и output notes. Исполнять на worker; cancellation через runtime budget/interrupt, не бесконтрольный background thread.

Sandbox считать ограниченной capability-моделью, не обещанием OS-grade isolation. При невозможности гарантировать bounded execution использовать отдельный worker process до включения user scripts.

API:
- immutable note snapshot и selection;
- PPQ, Pattern length, time range;
- seeded random service;
- schema-driven parameter form;
- возврат NotePatch или SelectionPatch.

Script не изменяет domain напрямую. Host валидирует IDs, range, pitch, velocity, output count и применяет patch одной командой.

Ошибки/timeout/cancel не меняют проект. Показать diagnostic без полного crash приложения.

Open script folder открывает пользовательский каталог. Не запускает scripts автоматически. При загрузке проекта не исполнять scripts из assets.

Run last script again повторяет последний успешный script с последними parameters и seed на текущем scope, не на старом snapshot. При изменении script hash требовать повторное подтверждение параметров. До первого запуска команда disabled.

#### Arpeggiator

Script wrapper для Arpeggiate: те же параметры, семантика и validation. Обеспечить идентичность результата с built-in для одинакового input.

#### Euclidean

Генератор равномерно распределённых hits.

Параметры: steps 1..128, pulses 0..steps, rotation, step ticks, pitch, velocity, gate, range, insertion mode.

Использовать собственную детерминированную реализацию Bjorklund; normal form начинается с hit при pulses > 0, rotation применяется после нормализации. Добавить golden tests для закреплённого tie/order policy. pulses=0 — silence; pulses=steps — каждый step.

#### Humanize

Небольшие seeded вариации start, duration и velocity. Pitch по умолчанию не меняется. Параметры в ticks и MIDI velocity units. Optional preserve chord alignment применяет один timing offset ко всей onset group. Ноты не выходят за Pattern boundaries.

#### Note Repeater Sprinkler

Разделяет выбранные notes на repeat cells заданного step и оставляет каждую с probability 0..1.

Параметры: step, probability, gate, velocity variation, seed. При probability=1 эквивалентен Chop с теми же step/gate без variation; probability=0 удаляет processed notes только после явного preview/Apply. Неполная последняя cell допускается.

#### Select by Velocity and More

Меняет только selection.

Фильтры: velocity min/max, pitch min/max, duration min/max, start range. Predicate — AND включённых фильтров.

Режимы: Replace / Add / Subtract / Intersect. Candidate set — все ноты текущего канала либо selection по явному переключателю. Ноты и project dirty state не менять.

### 18.23. Архитектура и тестирование

Выделить чистый note-tools module, независимый от UI/audio device. Вход: immutable snapshot, scope, versioned settings. Выход: validated NotePatch/SelectionPatch/AutomationPatch + diagnostics.

Unit/property tests:
- identity settings;
- empty input;
- selection isolation;
- duration >= 1;
- Pattern boundaries;
- pitch/velocity limits;
- stable seeded output;
- no duplicate IDs;
- transactional Apply/Cancel;
- undo/redo round-trip;
- source Pattern edits отражаются на всех clips;
- freeze/recompute preview не накапливает преобразования;
- script error/timeout/output overflow не меняет проект;
- parameter schema migration.

UI tests: dialog preview, focus, shortcuts, detached Piano Roll, Cancel через закрытие окна.

### 18.24. Этапы дополнения

- 0.2-J: общий tool framework, Quick Legato, оба Quick Quantize, Quick Chop, Glue, Flip.
- 0.2-K: Articulate, Quantize, Chop, Strum, Flam, Limit, Scale Levels.
- 0.2-L: Arpeggiate, Chord Progression, Randomize, Claw Machine, Riff Machine.
- 0.2-M: LFO после готовности Automation.
- 0.2-N: script runtime, команды Scripts и пять встроенных scripts.

Не блокировать разработку audio clips реализацией генераторов: сначала завершить фундамент 0.2-A/0.2-B.

### 18.25. Дополнительные источники требований

- Пользовательский скриншот меню Piano Roll Tools.
- Public Piano Roll Menu: https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/pianoroll_menu.htm
- Claw Machine: https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/pianoroll_claw.htm
- LFO: https://cluster.image-line.com/fl-studio-learning/fl-studio-online-manual/html/pianoroll_lfo.htm
- Riff Machine: https://cluster.image-line.com/fl-studio-learning/fl-studio-online-manual/html/pianoroll_riff.htm
- Public scripting API reference: https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/pianoroll_scripting_api.htm

Источники используются для определения назначения инструментов. Конкретные параметры, алгоритмы, ограничения и API этого раздела — самостоятельные проектные решения.
