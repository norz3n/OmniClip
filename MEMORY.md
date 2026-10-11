# OmniClip — Память состояния проекта (MEMORY.md)

## 1. Текущий статус
* **Фаза**: Реализация базового конвейера (Этапы 1–5) полностью завершена и верифицирована.
* **Целевая платформа**: Windows 10 (1903+) / Windows 11 (x86_64).
* **Язык / Тулчейн**: Rust 1.98.1 (2021 edition), Win32 API через crate `windows` (v0.58).
* **Статус сборки и тестов**: `cargo check --lib` (OK), `cargo test` (OK, 2/2 unit-теста пройдены).

---

## 2. Принятые ключевые решения и архитектура
1. **Zero-Copy захват и энкодинг**:
   - `IDXGIOutputDuplication` получает текстуру `ID3D11Texture2D` в VRAM (формат `DXGI_FORMAT_B8G8R8A8_UNORM`).
   - Используется двойная буферизация в VRAM для немедленного вызова `ReleaseFrame()`, исключающая задержки рабочего стола DWM.
   - Текстура передаётся напрямую в аппаратный MFT-энкодер через `MFCreateDXGISurfaceBuffer` и `IMFDXGIDeviceManager` без копирования на CPU.
2. **Аппаратный энкодер**:
   - Активирован аппаратный Intel Quick Sync Video H.264 Encoder MFT (`MFT_CATEGORY_VIDEO_ENCODER`, `MFT_ENUM_FLAG_HARDWARE`).
   - Асинхронный MFT разблокирован через `MF_TRANSFORM_ASYNC_UNLOCK = 1`.
   - Опрос событий переведён в неблокирующий режим с флагом `MF_EVENT_FLAG_NO_WAIT = 1` (`MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(1)`), что предотвращает зависание основного цикла захвата при статичном экране.
   - Поддерживается автоматическое подтверждение изменения формата потока (`MF_E_TRANSFORM_STREAM_CHANGE`).
3. **Кольцевой буфер (Ring Buffer)**:
   - Потокобезопасная структура `RingBuffer` (`parking_lot::Mutex`).
   - Инвариант: буфер всегда начинается с ключевого IDR-кадра (SPS/PPS). Не-ключевые кадры до первого IDR отбрасываются.
   - Обрезка старых данных происходит строго по границам ключевых кадров (`IDR-pruning`), синхронно отсекая устаревшие аудиосэмплы.
   - Мгновенное снятие слепка (`snapshot()`) для фонового сохранения (< 0.1 мс).
4. **Аудио**:
   - Поток `WasapiLoopbackCapture` захватывает системный звук с дефолтного устройства вывода (Speakers/Headphones).
   - Поддержка IEEE 32-bit Float и 16-bit PCM с нормализацией и заполнением тишины.
   - Синхронное сжатие в AAC через Media Foundation MFT (`AACMFTEncoder`, 48 kHz Stereo 192 kbps).
   - Привязка таймстемпов по Windows QPC к единой шкале времени с видео.
5. **Сброс на диск (MP4 Muxer)**:
   - Выполняется в отдельном фоновом потоке `omniclip-disk-writer`, не блокируя конвейер захвата.
   - `Mp4Muxer` через `IMFSinkWriter` упаковывает уже сжатые NALU и AAC в `.mp4` контейнер без единого перекодирования.
6. **Интерактивный рабочий стол**:
   - Реализована функция `D3D11Context::ensure_interactive_desktop()`, переключающая вызывающий поток на десктоп `Default` станции `WinSta0`. Это решает проблему ошибки `E_ACCESSDENIED (0x80070005)` в изолированных средах и службах.

---

## 3. Выполненные задачи
- [x] Проектирование архитектуры и спецификаций в `specs/`.
- [x] Настройка `Cargo.toml` с необходимыми Win32 и Media Foundation зависимостями.
- [x] Модуль высокоточного таймера: `src/platform/time.rs` (QPC -> HNS).
- [x] Модуль глобального хоткея: `src/platform/hotkey.rs` (`RegisterHotKey`, `Alt+F10`).
- [x] Модуль Direct3D 11: `src/capture/d3d11_device.rs` (`D3D11Context`, BGRA, Video Support, adapter binding).
- [x] Модуль захвата экрана: `src/capture/dxgi_duplicator.rs` (`DxgiDuplicator`, 1920x1080 @ 60 FPS, двойная буферизация).
- [x] Модуль Media Foundation Runtime: `src/encoder/mf_runtime.rs` (`MfPlatformGuard`, `DxgiDeviceManager`).
- [x] Модуль аппаратного видеоэнкодера: `src/encoder/h264_mft.rs` (`H264Encoder`, Zero-Copy DXGISurfaceBuffer, неблокирующий poll).
- [x] Модуль кольцевого буфера: `src/buffer/chunk.rs`, `src/buffer/ring_buffer.rs` (IDR alignment, unit tests).
- [x] Модуль аудиозахвата и кодирования: `src/audio/wasapi_loopback.rs`, `src/audio/aac_mft.rs`.
- [x] Модуль быстрого MP4-муксера: `src/muxer/mp4_sink.rs` (`Mp4Muxer` через `IMFSinkWriter`).
- [x] Основной пайплайн приложения: `src/main.rs` (многопоточная координация, фоновый Disk Writer, graceful shutdown).

---

## 4. Следующие шаги (Next Steps)
1. **Индикатор в системном трее (System Tray Icon)**:
   - Добавить иконку в трей Windows с индикацией статуса (активна фоновая запись / сохранение).
   - Всплывающее уведомление Windows (Toast notification или Balloon tooltip) при сохранении повтора.
2. **Отображение аппаратного курсора (Hardware Cursor Rendering)**:
   - Опциональная отрисовка формы курсора мыши (`DXGI_OUTDUPL_POINTER_SHAPE_INFO`) прямо в текстуру кадра.
3. **Конфигурационный файл (omniclip.toml)**:
   - Загрузка пользовательских настроек (разрешение, FPS, битрейт, длительность буфера, клавиша сохранения).
