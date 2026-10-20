# Сборка (BLD-013)

Общее для всех ОС: `rustup`. Версия toolchain, компоненты и targets
берутся из `rust-toolchain.toml` автоматически при первой сборке.

Сейчас проект не использует bindgen, C/C++ код и сторонние SDK.
ASIO SDK и VST3 SDK появятся в Milestone 1A и P1; тогда сюда будут
добавлены их требования (включая `libclang`).

## Linux (reference: Ubuntu 24.04)

Сборка:

```sh
sudo apt-get install -y libasound2-dev libjack-jackd2-dev pkg-config
```

Запуск GUI (runtime-библиотеки загружаются динамически):

```sh
sudo apt-get install -y libxkbcommon-x11-0 libgl1-mesa-dri
```

Без `libxkbcommon-x11` окно на X11 не откроется (panic при старте).
Это runtime-зависимость, её нужно указать в пакете (ADR-011).

Headless smoke test GUI: `xvfb-run -a ./target/release/jdance --smoke-test-frames 60`.

## Windows 11 x86_64

Visual Studio Build Tools (MSVC, Windows SDK). Дополнительных пакетов
нет: WASAPI входит в систему.

## macOS (Apple Silicon)

Xcode Command Line Tools: `xcode-select --install`.

## Проверено

| ОС | Сборка | Тесты | GUI smoke | Кто/когда |
|---|---|---|---|---|
| Ubuntu 24.04 x86_64 (контейнер) | PASS | PASS | PASS (Xvfb) | Claude Code, 2026-10-20 |
| Windows 11 x86_64 | NOT RUN | NOT RUN | NOT RUN | — (ждёт CI) |
| macOS Apple Silicon | NOT RUN | NOT RUN | NOT RUN | — (ждёт CI) |
