# SPEC: Mixer signal flow, Playlist, Built-in Effects

Источник: требования владельца продукта (2026-10-20), перенесены
дословно ниже разделителя.

Согласование с Amendment 02 (действует приоритет Amendment 02 там, где
он явно расширяет scope):
- sends и sidechain (Amendment 02, раздел 17) сохраняются; в схеме ниже
  они ответвляются в точках pre-/post-fader;
- встроенный компрессор с sidechain (MIX-040) сохраняется как пятый
  встроенный эффект;
- лимит insert slots приводится к 8 (вместо 10 в MIX-003);
- automation clips вне scope playlist MVP (см. ниже); автоматизация
  параметров (раздел 18 Amendment 02) пересматривается отдельно —
  архитектура parameter events с frame offset закладывается сейчас.

---

```
Instrument
    ↓
Mixer bus input sum
    ↓
Insert 1 → Insert 2 → ... → Insert 8
    ↓
Gain → Pan/Balance → Mute/Solo
    ↓
Bus meter
    ↓
Master input sum
    ↓
Master inserts
    ↓
Master gain
    ↓
Optional Safety Limiter
    ↓
Master output meter
    ↓
Audio device / Export
```

## Playlist

### Lane semantics

Lane — организационная дорожка.

Lane:
- не содержит инструмент;
- не определяет mixer routing;
- может содержать разные PatternClips;
- имеет name, color, mute, solo.

Порядок lanes влияет только на отображение.

Lane mute/solo управляет допуском событий в playback,
а не заглушает mixer bus целиком.

Если инструмент используется на нескольких lanes,
mute одной lane не должен заглушать остальные.

### Clip semantics

PatternClip ссылается на Pattern.

Clip duration:
pattern.length_ticks * repeat_count.

repeat_count >= 1.

Не хранить одновременно independent duration и repeat_count,
если между ними нет отдельной явно описанной семантики.

Clip interval:
[start, start + duration).

Ноты каждого повторения используют local pattern time.

### Editing

Поддержать:
- create;
- move;
- delete;
- duplicate;
- multi-select;
- изменение repeat_count правым краем;
- перенос между lanes;
- Make unique;
- mute clip;
- rename/color lane.

Duplicate:
- новый Clip ID;
- ссылка на тот же Pattern.

Make unique:
- копирует Pattern и его события с новыми ID;
- выбранный Clip ссылается на копию;
- остальные экземпляры не меняются.

Любая drag operation — одна undo command.

### Pattern length changes

Изменение длины Pattern:
- сохраняет repeat_count экземпляров;
- меняет вычисляемую длину всех связанных clips;
- не перемещает соседние clips автоматически;
- может создавать overlap.

При уменьшении длины:
- не удалять события молча;
- предложить cancel либо явную обрезку;
- обрезка входит в ту же undoable operation.

### Overlap

Перекрывающиеся clips допускаются:
- на одной lane;
- на разных lanes;
- с одним и тем же Pattern.

Все допущенные события воспроизводятся независимо.

Не дедуплицировать одинаковые ноты из разных clips.

### Playback boundaries

На конце повторения:
- завершить gated notes этого повторения;
- затем обработать начало следующего.

One-shot sampler:
- clip end не обрезает уже запущенный sample;
- Stop, seek и mute источника завершают его коротким release.

На seek:
- не восстанавливать ноты, начавшиеся до точки перехода;
- запускать только события от новой позиции;
- это ограничение MVP явно документировать.

На global loop wrap:
- завершить инструментальные голоса;
- сохранить tails эффектов;
- начать события нового loop interval.

### Mute/Solo

Если есть solo lanes:
- допускаются только solo lanes;
- mute имеет приоритет над solo.

Clip mute имеет приоритет над lane solo.

Mixer mute/solo применяется после генерации звука
и независимо от Playlist mute/solo.

### Arrangement end

Arrangement end:
максимальная правая граница всех clips,
включая muted clips.

При достижении конца:
- прекратить генерацию новых событий;
- завершить gated voices;
- обработать настроенный tail;
- остановить transport.

Explicit Stop:
- коротко погасить выход;
- очистить voices и effect state
  без слышимого скачка и без тяжёлой работы в callback.

### Out of scope

В MVP отсутствуют:
- audio clips;
- automation clips;
- arbitrary clip offsets;
- stretch;
- fractional pattern repetitions;
- nested arrangements;
- ripple editing.


## Built-in Effects

### General

Каждый mixer bus и master имеют до 8 insert slots.

Поддержать:
- add;
- remove;
- reorder;
- bypass;
- изменение параметров;
- сохранение настроек;
- undo/redo.

Процессоры имеют независимое runtime state.
Один instance нельзя разделять между realtime и export.

Не сохранять delay buffers, reverb buffers
или filter history в project.json.

Сохранять:
- effect type;
- stable instance ID;
- parameter schema version;
- parameters;
- bypass state.

### Processor lifecycle

Каждый процессор реализует:
- prepare(sample_rate, max_block_frames);
- process(stereo_buffer, parameter_events);
- reset();
- latency_frames();
- tail_description().

prepare:
- выделяет память;
- проверяет параметры;
- выполняется вне audio callback.

process:
- не выделяет и не освобождает память;
- поддерживает переменный размер блока;
- не обращается к UI.

reset:
- bounded operation либо подготовленная смена состояния;
- не очищает многомегабайтные буферы
  за один непредсказуемый callback.

### Structural edits

Добавление, удаление и перестановка эффектов:
- подготавливаются вне callback;
- публикуются на границе audio block;
- старое состояние освобождается вне callback.

В MVP не обещать бесшовное сохранение хвоста
при удалении или перестановке эффекта.

Применять короткий fade для подавления щелчков.
Ограничение по обрыву tail документировать.

### Parameters

Каждый параметр имеет:
- stable ID;
- unit;
- min/max;
- default;
- mapping;
- smoothing policy.

В persisted format использовать именованные параметры,
а не только позиции в массиве.

Полная timeline automation вне MVP.
Архитектура допускает parameter events с frame offset.

## Effect 1: Parametric EQ

Stereo-linked 4-band EQ:

1. Low shelf.
2. Bell.
3. Bell.
4. High shelf.

Параметры:
- frequency;
- gain: -24..+24 dB;
- Q для bell: 0.1..18;
- band bypass.

Frequency:
- нижняя граница 20 Hz;
- верхняя min(20 kHz, 0.45 * sample_rate).

Для shelf slope использовать фиксированное
документированное значение в MVP.

Требования:
- идентичная обработка L/R;
- устойчивая параметризация фильтров;
- smoothing изменений;
- график frequency response;
- отсутствие NaN/Inf при крайних параметрах.

Latency: 0 frames.

## Effect 2: Tempo Delay

Stereo delay.

Параметры:
- sync on/off;
- free time: 1..2000 ms;
- synced division: 1/16, 1/8, 1/4, 1/2,
  dotted 1/8, triplet 1/8;
- feedback: 0..0.95;
- wet: 0..1;
- feedback low-pass;
- ping-pong on/off.

Буфер выделяется под максимальную длительность
с учётом минимального поддерживаемого BPM.

Не ограничивать synced mode лимитом 2000 ms:
этот лимит относится только к free mode.

При смене времени:
- crossfade между read positions;
- не перемещать read pointer скачком.

Ping-pong:
- определить точную схему cross-feedback;
- покрыть impulse tests.

Latency: 0 frames.
Tail: зависит от delay time и feedback.

## Effect 3: Algorithmic Reverb

Собственный stereo algorithmic reverb.

Первая реализация:
- feedback delay network;
- фиксированная topology;
- положительные bounded delay lengths;
- устойчивое feedback mixing.

Параметры:
- decay: 0.2..10 seconds;
- damping;
- pre-delay: 0..100 ms;
- stereo width: 0..1;
- wet: 0..1.

Для MVP:
- topology и базовый room size фиксированы;
- пользовательский параметр room size не добавлять,
  если его изменение вызывает неподдерживаемую
  перестройку delay network.

Decay означает целевую оценку RT60,
а не гарантированную точную характеристику.

Требования:
- bounded feedback;
- отсутствие самовозбуждения;
- корректное затухание;
- sample-rate-aware coefficients;
- отсутствие allocations при изменении decay/damping.

Latency:
0 frames на dry path.

Pre-delay:
часть эффекта wet path, не plugin latency.

## Effect 4: Safety Limiter

Назначение:
защита выхода от sample peaks.

Это не true-peak limiter.

Параметры:
- enabled;
- ceiling: -12..0 dBFS;
- release: 10..500 ms.

Реализация MVP:
- stereo-linked peak detection;
- мгновенное уменьшение gain;
- сглаженное восстановление gain;
- без lookahead;
- без oversampling;
- без makeup gain.

Latency: 0 frames.

Ограничения:
- возможны искажения на сильных пиках;
- intersample peaks не контролируются;
- не рекламировать как mastering limiter.

При enabled:
- каждый конечный output sample должен быть
  ограничен ceiling с учётом численного допуска.

Показывать gain reduction meter.

Limiter — отключаемый master output stage.
Не включать скрытую нормализацию или clipping
в другие стадии микшера.

## Bypass and tails

Bypass переключать через короткий crossfade.

При полном bypass:
- tail может быть погашен;
- state сбрасывается безопасным способом;
- повторное включение начинается с чистого state.

Это hard-bypass semantics с плавным переходом,
а не сохранение скрытого бесконечного tail.

## Export

Эффекты в export используют:
- ту же DSP-реализацию;
- отдельные instances;
- тот же порядок processing;
- одинаковые параметры.

Настройку export tail изменить:
- default: 5 seconds;
- user-configurable;
- при ожидаемом более длинном tail показать предупреждение.

Default 5 seconds не гарантирует полный хвост
при больших feedback/decay.

Float32 export:
- не применять скрытый clamp;
- если Safety Limiter включён пользователем,
  его действие сохраняется в render.

## DSP tests

Для EQ:
- unity response при gain = 0;
- response около заданных частот;
- stability на extreme parameters.

Для Delay:
- положение первого echo;
- feedback decay;
- sync при разных BPM;
- ping-pong channel sequence.

Для Reverb:
- impulse response;
- bounded output;
- decay;
- silence после затухания в пределах tolerance.

Для Limiter:
- ceiling;
- stereo linking;
- release;
- корректность при резких transient.

Для всех:
- разные sample rates;
- разные block sizes;
- отсутствие NaN/Inf;
- reset;
- bypass transitions;
- realtime/offline equivalence в пределах tolerance.
