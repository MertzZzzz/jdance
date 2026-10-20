# Realtime-структуры (RT-010)

Всё, что трогает audio thread. Свойства относятся к операциям,
которые выполняются на audio thread.

| Структура | Направление | Где | Alloc-free в RT | Lock-free | Wait-free | Время |
|---|---|---|---|---|---|---|
| Команды `Command` | UI → RT, SPSC, ёмкость 256 | `engine` (`rtrb`) | да | да | да | O(1) pop; ≤ 64 команд за `render` |
| Возврат мусора `Garbage` | RT → UI, SPSC, ёмкость 256 | `engine` (`rtrb`) | да | да | да | O(1) push |
| `PlaybackSnapshot` | UI → RT через очередь команд, `Box` | `engine` | да (только перемещение указателя) | да | да | замена: O(voices × channels) для перепривязки голосов |
| `EngineFeedback` | RT → UI, атомики | `engine` | да | да | да, кроме `peak_load` (CAS-цикл, lock-free) | O(1) |
| `StreamStats` | RT → UI, атомики | `audio-api` | да | да | да | O(1) |
| `StreamStats::error` | backend error callback → UI, `Mutex` | `audio-api` | нет | нет | нет | **не** вызывается в render callback |
| Голоса `Voice` | только RT | `engine` | да (128 слотов при создании) | — | — | O(voices) на блок |
| Буферы каналов и шин | только RT | `engine` | да (256 каналов, 129 шин × вход/pre/post, по 1024 фрейма; ~5 МБ) | — | — | O(каналы + шины) на блок |
| Состояние в snapshot (гейны, delay lines, компрессор) | UI → RT вместе со snapshot | `engine` | да: перенос обменом буферов (`take_state_from`) | — | — | O(n²) по объектам при замене, n ≤ сотни |
| Ожидающий snapshot (`pending`) | только RT | `engine` | да | — | — | замена на границе блока после структурного fade |
| Reset эффектов | только RT | `engine`, `dsp` | да | — | — | ≤ 16 384 сэмплов буфера на insert за блок |
| Метры (`Meters`) | RT → UI, атомики | `engine` | да | да | да | O(шины) на блок |
| Команда `SetParam` | UI → RT | `engine` | да | да | да | поиск insert O(inserts) |
| Scratch-буферы | только RT | `engine`, `audio-cpal` | да (1024 и 8192 фреймов) | — | — | — |

## Инварианты

- Snapshot не освобождается на audio thread: старый уходит в очередь
  мусора, и процессор берёт новый snapshot только при свободном месте
  в этой очереди (иначе ждёт следующего callback, счётчик
  `deferred_snapshots`).
- Голоса не держат `Arc` на сэмплы: они хранят индекс канала, а буфер
  берут из текущего snapshot. На audio thread нет изменения счётчиков
  `Arc`.
- Callback любой длины делится на блоки ≤ `MAX_BLOCK` = 1024 (RT-020).
- Panic в render: `catch_unwind` в адаптере, дальше тишина и флаг
  `panicked` (RT-031). Payload panic не освобождается на audio
  thread (`mem::forget`).

## Проверка

- `engine/tests/rt_alloc.rs`: 400 вызовов `render` с заменой snapshot,
  stop/play/panic и разными размерами блоков под guard-аллокатором
  (`rt-check`) дают 0 heap-операций (RT-001). Граф включает треки,
  send, sidechain и компрессор с lookahead, так что проверен и перенос
  состояния при замене snapshot.
- `engine/tests/render.rs`: одинаковый результат бит в бит при случайных
  размерах блоков от 1 до 2048 (RT-022).

## Известные ограничения

- `Instant::now()` в `render` (замер нагрузки): системный вызов
  без аллокаций, но время выполнения зависит от ОС.
- Panic hook стандартной библиотеки пишет в stderr при panic на audio
  thread. Это путь аварии, а не штатной работы.
- Приоритет audio thread (RT-040) пока задаёт сам backend/CPAL;
  явная настройка MMCSS / time-constraint / SCHED_FIFO не сделана.
