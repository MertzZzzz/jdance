# CPAL 0.18.2: возможности по backend'ам (BLD-030)

Источник: исходники cpal 0.18.2 и проверка в этом репозитории.
Пункты без проверки на железе помечены NOT RUN.

| Возможность | WASAPI | CoreAudio | ALSA | JACK |
|---|---|---|---|---|
| Перечисление устройств, стабильный ID | да (API) | да (API) | да, проверено | да (API) |
| Список sample rate | диапазоны | диапазоны | диапазоны, проверено (`null`: 1…u32::MAX) | фиксирован сервером |
| Диапазон buffer size | зависит от режима [NOT RUN] | да [NOT RUN] | да, проверено | фиксирован сервером |
| Fixed buffer size из приложения | [NOT RUN] | [NOT RUN] | да, проверено (256 на `null`) | нет |
| Reported output latency | нет | нет | нет | нет |
| Driver control panel | нет | нет | нет | нет |
| Уведомление о смене устройства | ошибка stream | ошибка stream | ошибка stream | ошибка stream |

Следствия:
- `DeviceCapabilities::reports_latency = false` и `has_control_panel = false`
  для всех CPAL-backend'ов; UI это показывает (AUD-011).
- ASIO-host реализуется отдельным адаптером `audio-asio` (ADR-006):
  control panel и `kAsioResetRequest` через CPAL недоступны.
- В cpal 0.18 есть нативный PipeWire host (feature `pipewire`); не включён (AUD-001).

## ASIO (feature `asio`, crate `asio-sys` 0.4.0) — ASIO-011

Источник: исходники cpal 0.18.2 (`src/host/asio/*`) и asio-sys 0.4.0
(`src/bindings/mod.rs`, `build.rs`), 2026-10-22. На железе не проверялось.

| Возможность | Есть в CPAL | Источник |
|---|---|---|
| Перечисление драйверов | да, по имени из реестра | `asio-sys` `driver_names()` |
| Стабильный ID драйвера | нет: ID — имя драйвера, CLSID наружу не выдаётся | `host/asio/device.rs` `id()` |
| Имена каналов драйвера (`ASIOGetChannelInfo`) | нет: используется только для формата канала 0 | `bindings/mod.rs` `asio_channel_info` |
| Sample rates | да: `can_sample_rate` по стандартному ряду | `bindings/mod.rs` |
| Buffer constraints (min, max, preferred, granularity) | да, `buffersize_range()` | `bindings/mod.rs` |
| Latency входа и выхода | да, используется для временных меток | `host/asio/stream.rs` |
| Driver control panel (`ASIOControlPanel`) | нет | в API `Driver` метода нет |
| `kAsioResetRequest` | ошибка потока `StreamInvalidated`, приложение переоткрывает само | `host/asio/stream.rs` |
| `kAsioBufferSizeChange` | не поддерживается (asio-sys отвечает «не поддерживаю») | `bindings/mod.rs` |
| `kAsioOverload` | нет | — |
| Вход и выход в одном `bufferSwitch` | да, общий буферный обратный вызов драйвера | `host/asio/stream.rs` |
| `bufferSwitchTimeInfo`, `ASIOOutputReady` | да | `bindings/mod.rs` |
| Сборка | LLVM/libclang (bindgen), компиляция C++ SDK (`cc`); SDK скачивается **во время сборки** с сайта Steinberg, если не задан `CPAL_ASIO_DIR` | `build.rs` |

Вывод для ADR-006: через CPAL нет панели драйвера, имён каналов и
стабильного ID; сборка зависит от сети и libclang. Решение — собственный
адаптер (ADR-006).
