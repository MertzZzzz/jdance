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
