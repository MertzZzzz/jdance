# Источники и provenance (CR-003, CR-006, LIC-008)

## Источники требований к поведению

- Собственная спецификация: `docs/spec/`.
- Общие знания о DAW и публичная документация форматов
  (RIFF/WAVE, VST3 SDK, ASIO SDK — по мере подключения).

Материалы из CR-002 (бинарники, ресурсы, внутренние документы FL Studio)
не используются.

## Ассеты

| Файл | Происхождение | Лицензия |
|---|---|---|
| `assets/demo/samples/*.wav` | синтезированы кодом `cargo xtask gen-demo` | GPL-3.0-or-later (как репозиторий) |
| `assets/demo/demo.jdance` | создан `cargo xtask gen-demo` | GPL-3.0-or-later |
| `crates/test-support/golden/*.wav` | результат рендера тестов | GPL-3.0-or-later |
