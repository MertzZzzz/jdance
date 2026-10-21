# ADR-015: Встроенные эффекты, bypass, мастер-цепочка

- Статус: ACCEPTED
- Дата: 2026-10-20
- Требования: `docs/spec/spec-mixer-playlist-effects.md` (Built-in Effects), MIX-033, MIX-040

## Общая модель

- Параметры описаны схемой в `domain` (`EffectKind::params`): стабильный
  ID, единица, min/max, default, mapping (linear/log/toggle/choice),
  smoothing (step или ramp 20 мс). В проекте хранится
  `{kind, schema, params{id: value}}`; отсутствующие параметры берут
  default, неизвестные — ошибка загрузки.
- Процессоры в `dsp` реализуют `Processor`: `prepare` (аллокации, вне
  callback) → `set_param` / `process_block` / `reset_step` (RT-safe).
  Порядок индексов параметров совпадает со схемой — проверяется тестом.
- `process_with_events` применяет parameter events с frame offset
  (архитектура для будущей автоматизации).
- Правка, меняющая только параметры (без latency), уходит в движок
  командой `SetParam` без пересборки snapshot; остальное — пересборка
  с переносом состояния по ID.

## Решения, оставленные спецификацией на реализацию

| Вопрос | Решение |
|---|---|
| Mix wet/dry (Delay, Reverb) | `out = (1 − wet)·dry + wet·effect` |
| Shelf slope EQ | RBJ `S = 1`, фиксировано |
| Фильтры EQ | RBJ biquad, TDF-II, состояние `f64`; пересчёт коэффициентов каждые 16 сэмплов во время ramp (глобальный счётчик → независимость от блоков) |
| Band bypass EQ | ramp gain к 0 dB (biquad = identity) |
| Delay: буфер | 6 с = 1/2 при минимальном темпе 20 BPM |
| Delay: смена времени | crossfade 50 мс между двумя точками чтения |
| Ping-pong | моно-сумма входа → левая линия; левая → правая, правая → левая; эхо L, R, L, … с усилением `fb^(k−1)` |
| Reverb | FDN 8 линий 29.7–73.1 мс, Householder `I − 2/N·11ᵀ`, one-pole damping (коэффициент ≤ 0.85), `g = 10^(−3·len/(sr·RT60))`, mid/side width |
| Limiter | stereo-linked sample peak, gain ≤ ceiling/peak, recovery one-pole по release, финальный clamp на 1 ulp |
| Bypass | hard bypass: crossfade 10 мс wet→dry, затем reset по 16 384 сэмпла буфера за блок; повторное включение ждёт чистого состояния и делает crossfade обратно; dry задержан на latency эффекта |
| Структурные правки | двухфазная замена snapshot: fade-out 5 мс затронутых треков → замена на границе блока → fade-in; хвосты эффектов на этих треках обрываются (ограничение MVP) |
| Stop | fade-out выхода 5 мс → голоса очищаются, все эффекты получают ограниченный reset, лимитер сбрасывается |
| Метры | пик по шине после mute/solo (до PDC-задержки), выход после лимитера, минимальный gain лимитера; атомики `fetch_max`/`fetch_min` по битам `f32` |

## Ограничения

- Decay ревербератора — оценка RT60: на тесте 1 с даёт ≈ 0.91 с
  (damping добавляет затухание ВЧ).
- Limiter не контролирует intersample peaks.
- Изменение pre-delay ревербератора применяется скачком (Smoothing::Step).
- Компрессор остаётся пятым встроенным эффектом (MIX-040, sidechain).
