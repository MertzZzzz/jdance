# ADR-005: Panic strategy

- Статус: ACCEPTED
- Дата: 2026-10-20
- Требования: RT-030, RT-031, RT-032

## Решение

- `panic = "unwind"` в release (`Cargo.toml`), чтобы работал `catch_unwind`.
- Audio callback адаптера оборачивает вызов движка в `catch_unwind`.
  При panic: callback выводит тишину до перезапуска stream, ставит
  флаг `panicked`, UI показывает ошибку и предлагает переоткрыть
  устройство.
- Payload panic не освобождается на audio thread (`mem::forget`).
- Все будущие `extern "C"`/`extern "system"` точки входа (ASIO, VST3,
  собственный драйвер) обязаны иметь такой же барьер.

## Последствия

- Segfault в стороннем коде (плагин, драйвер) так не перехватить;
  для этого autosave/recovery (PER-020) и out-of-process scan (PLG-022).
