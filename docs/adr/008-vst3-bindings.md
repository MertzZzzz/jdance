# ADR-008: VST3 bindings

- Статус: ACCEPTED
- Дата: 2026-10-20
- Требования: PLG-005, PLG-006, PLG-010, BLD-011

## Контекст

Хосту нужны Rust-типы интерфейсов VST3 (`IPluginFactory`, `IComponent`,
`IAudioProcessor`, `IEditController`, …) и возможность реализовать
host-интерфейсы (`IHostApplication`, `IComponentHandler`, `IBStream`,
`IParameterChanges`, …) на Rust. Лицензия приложения — GPL-3.0-or-later.

## Варианты

1. Крейт `vst3` 0.3.0 (coupler-rs/vst3-rs): bindings, сгенерированные
   из заголовков VST 3 SDK 3.8 и опубликованные в исходниках крейта
   (SDK при сборке не нужен); одинаковые на Windows, macOS и Linux;
   лицензия MIT OR Apache-2.0. Зависимость `com-scrape-types` 0.1.1
   (MIT OR Apache-2.0) даёт `ComPtr`/`ComRef` и реализацию COM-классов
   на Rust (`Class`, `ComWrapper`).
2. `vst3-sys` (GPLv3): ручные bindings старой версии SDK, без
   активной поддержки.
3. Собственный генератор bindings из закреплённой версии SDK.

## Решение

Вариант 1: `vst3` 0.3.0. Он поддерживается, соответствует SDK 3.8
(лицензия SDK — MIT, PLG-005) и не требует копии SDK в CI.
Весь `unsafe` для VST3 изолирован в `crates/plugin-vst3` и тестовом
плагине `test-plugins/jdance-test-fx` (workspace-lint
`unsafe_code = "deny"` снят только там, с комментарием).
VST3-типы не выходят за пределы `plugin-vst3` (PLG-010): наружу
отдаются типы `plugin-api`.

Идентификатор класса в проекте — канонический hex `FUID::toString`
(четыре 32-битных слова). На Windows байты `TUID` имеют раскладку COM
`GUID`, поэтому преобразование учитывает платформу; проект, сохранённый
на одной ОС, находит плагин на другой.

Название «VST3» в UI — только как техническое название формата, без
логотипа (PLG-007).

## Последствия

- Обновление SDK = обновление крейта `vst3`; бинарная совместимость
  интерфейсов VST3 стабильна.
- Абстракций над API в крейте нет; lifecycle, буферы, очереди и
  потокобезопасность реализует `plugin-vst3`.
- Тестовый плагин написан на тех же bindings, поэтому ошибки в них
  могут маскироваться. Проверка на реальных плагинах — Milestone P3
  (PLG-090).
