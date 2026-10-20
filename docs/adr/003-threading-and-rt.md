# ADR-003: Модель потоков и RT-структуры

- Статус: ACCEPTED (для Milestone 1; пересматривается при добавлении mixer graph и VST3)
- Дата: 2026-10-20
- Требования: ARCH-003, ARCH-004, RT-001…RT-031

## Решение

- UI thread владеет документом (`Project`) и историей правок.
- На каждое изменение non-RT код компилирует неизменяемый
  `PlaybackSnapshot` (позиции событий уже в сэмплах, сэмплы уже
  в sample rate устройства) и отправляет его в `EngineProcessor`
  через wait-free SPSC очередь (`rtrb`).
- Старый snapshot возвращается второй SPSC очередью и освобождается
  в UI thread (`EngineHandle::collect_garbage`).
- Состояние для UI — relaxed-атомики (`EngineFeedback`, `StreamStats`).
- Offline render использует тот же `EngineProcessor` без устройства.
- Позиции событий — абсолютные (от начала цикла), поэтому результат
  не зависит от границ блоков.

Таблица свойств структур: `docs/realtime-structures.md`.

## Отклонённые варианты

- `Arc<Mutex<Project>>`, разделяемый с audio thread: lock в callback.
- `AtomicPtr` swap snapshot с ручным reclamation: больше `unsafe`
  без выигрыша — очередь даёт и передачу, и возврат.

## Последствия

- Каждое изменение документа пересобирает snapshot целиком. Для
  step sequencer это дёшево; для playlist/piano roll может
  понадобиться инкрементальная компиляция.
- Голоса перепривязываются к каналам по `ChannelId` при замене
  snapshot: O(voices × channels).
