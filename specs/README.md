# Инструкция для AI-агента (Handoff Context)

Данная директория содержит исчерпывающие спецификации для реализации **OmniClip** — сверхлёгкого аналога NVIDIA ShadowPlay / AMD ReLive на Rust под Windows.

## Как использовать эти спецификации:
1. `01_ARCHITECTURE.md` — Общая архитектура, Zero-Copy конвейер, модель многопоточности и тайминги.
2. `02_CAPTURE_DXGI.md` — Модуль захвата экрана на Direct3D 11 и DXGI Desktop Duplication.
3. `03_ENCODER_MFT.md` — Аппаратное кодирование через Windows Media Foundation MFT (NVENC / AMF / QSV).
4. `04_RING_BUFFER.md` — Структура данных кольцевого буфера в RAM и алгоритм отсечения по IDR-кадрам.
5. `05_AUDIO_WASAPI.md` — Захват системного звука (WASAPI Loopback) и AAC-кодирование.
6. `06_MUXER_MP4.md` — Упаковка H.264 + AAC в MP4 без перекодирования.
7. `07_AGENT_PROMPT.md` — Готовый системный промпт для передачи агенту в новом чате для старта разработки.
8. `08_UI_UX_OVERLAY.md` — UI/UX спецификация оверлея в стиле ShadowPlay/ReLive на базе Material Design 3.

## Главные инженерные принципы:
* **Zero-Copy**: Никакого копирования `ID3D11Texture2D` в системную память (RAM) для последующего кодирования. Текстура передаётся напрямую в аппаратный энкодер через `IMFDXGIDeviceManager`.
* **Fail-Fast**: Ошибки COM и DXGI обрабатываются явно. Устройство D3D11 пересоздаётся автоматически при `DXGI_ERROR_DEVICE_REMOVED` или смене разрешения.
* **Строгая типизация**: Без костылей с `unsafe` там, где есть безопасные обёртки Windows crate.
