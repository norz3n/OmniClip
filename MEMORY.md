# OmniClip — Память состояния проекта (MEMORY.md)

## 1. Текущий статус
* **Фаза**: Реализация игрового оверлея и панели управления (Material Design 3 / specs 08) полностью завершена и интегрирована.
* **Целевая платформа**: Windows 10 (1903+) / Windows 11 (x86_64).
* **Язык / Тулчейн**: Rust 1.98.1 (2021 edition), Win32 API через crate windows (v0.58).
* **Статус сборки и тестов**: cargo check --all-targets (OK, 0 warnings), cargo test (OK, 14/14 unit-тестов пройдены).

---

## 2. Принятые ключевые решения и архитектура
1. **Zero-Copy захват и энкодинг**:
   - IDXGIOutputDuplication получает текстуру ID3D11Texture2D в VRAM (формат DXGI_FORMAT_B8G8R8A8_UNORM).
   - Используется двойная буферизация в VRAM для немедленного вызова ReleaseFrame(), исключающая задержки рабочего стола DWM.
   - Текстура передается напрямую в аппаратный MFT-энкодер через MFCreateDXGISurfaceBuffer и IMFDXGIDeviceManager без копирования на CPU.
2. **Аппаратный энкодер**:
   - Активирован аппаратный Intel Quick Sync Video H.264 Encoder MFT.
   - Асинхронный MFT разблокирован через MF_TRANSFORM_ASYNC_UNLOCK = 1.
   - Опрос событий переведен в неблокирующий режим с флагом MF_EVENT_FLAG_NO_WAIT = 1.
   - Поддерживается автоматическое подтверждение изменения формата потока.
3. **Кольцевой буфер (Ring Buffer)**:
   - Потокобезопасная структура RingBuffer (parking_lot::Mutex).
   - Инвариант: буфер всегда начинается с ключевого IDR-кадра (SPS/PPS). Не-ключевые кадры до первого IDR отбрасываются.
   - Обрезка старых данных происходит строго по границам ключевых кадров (IDR-pruning), синхронно отсекая устаревшие аудиосэмплы.
   - Мгновенное снятие слепка (snapshot()) для фонового сохранения (< 0.1 мс).
   - Динамическое изменение длины буфера (set_max_duration_hns) прямо из UI-оверлея с немедленной обрезкой.
4. **Аудио (Системный звук + Микрофон)**:
   - WasapiLoopbackCapture захватывает системный звук с дефолтного устройства вывода (eRender).
   - Захват микрофона: опциональный опрос дефолтного устройства записи (eCapture).
   - In-place микшер с настройкой громкости mic_volume и clamp(-1.0, 1.0).
   - Генератор тишины (Gap-Filler): 10-мс блоки тишины при простое WASAPI Loopback для предотвращения десинхронизации A/V.
   - Нормализация формата в стерео 48 000 Гц.
   - Сжатие в AAC через Media Foundation MFT (AACMFTEncoder, 48 kHz Stereo 192 kbps).
5. **Сброс на диск (MP4 Muxer)**:
   - Выполняется в фоновом потоке omniclip-disk-writer, не блокируя захват.
   - Mp4Muxer через IMFSinkWriter упаковывает NALU и AAC в .mp4 без перекодирования.
6. **Интерактивный рабочий стол**:
   - D3D11Context::ensure_interactive_desktop() переключает вызывающий поток на десктоп Default станции WinSta0.
7. **Пользовательский интерфейс и оверлей (Material Design 3 / egui & eframe)**:
   - Миграция на `egui` / `eframe` (v0.31, glow/winit бэкенд) с полным сохранением визуального стиля Material Design 3 Dark Theme.
   - Устранение корневой причины зависания: отказ от синхронных оконных процедур Win32 GDI WndProc с реентерабельными дедлоками мьютекса в пользу реактивной однопоточной модели `eframe::App` (`OmniClipApp`), работающей на главном потоке.
   - Поток захвата и энкодинга вынесен в выделенный фоновый поток `omniclip-capture`.
   - Hub-экран (Alt+Z): полноэкранный полупрозрачный Scrim backdrop (alpha ~85%), центрированное диалоговое окно Control Hub с 3 карточками (Instant Replay, Manual Record, Quick Settings), динамический расчет ОЗУ кольцевого буфера, переключатели качества (60/120 FPS, 15/35/50 Mbps), микрофона и кнопка открытия папки записей.
   - In-Game HUD: компактный semi-transparent бейдж в верхнем правом углу с пульсирующим зеленым/красным индикатором статуса, счетчиком FPS и плашкой 'SAVING...'.
   - Toast-уведомления: стилизованный Snackbar в нижнем правом углу при сохранении повтора с именем файла, размером и кнопкой немедленного открытия в Проводнике через `explorer /select, <path>`. Автоскрытие через 4 секунды.
   - Управление окном оверлея: прозрачный фон (`transparent: true`), `decorations: false`, `always_on_top: true`. При закрытом оверлее и отсутствии активных тостов окно автоматически переходит в `Visible(false)` и `MousePassthrough(true)` для беспрепятственного геймплея.
   - Глобальные хоткеи: обработка Alt+F10 (сохранить повтор), Alt+Z (оверлей Hub) и Alt+F9 (ручная запись) через crossbeam-канал.

---

## 3. Выполненные задачи
- [x] Проектирование архитектуры и спецификаций в specs/.
- [x] Настройка Cargo.toml с необходимыми Win32 и Media Foundation зависимостями.
- [x] Модуль высокоточного таймера: src/platform/time.rs (QPC -> HNS).
- [x] Модуль глобального хоткея: src/platform/hotkey.rs (RegisterHotKey, Alt+F10, Alt+Z, Alt+F9).
- [x] Модуль Direct3D 11: src/capture/d3d11_device.rs.
- [x] Модуль захвата экрана: src/capture/dxgi_duplicator.rs (DxgiDuplicator, 1920x1080 @ 60 FPS, двойная буферизация).
- [x] Модуль Media Foundation Runtime: src/encoder/mf_runtime.rs.
- [x] Модуль аппаратного видеоэнкодера: src/encoder/h264_mft.rs (Zero-Copy DXGISurfaceBuffer).
- [x] Модуль кольцевого буфера: src/buffer/ring_buffer.rs (IDR alignment, динамический set_max_duration_hns, unit tests).
- [x] Модуль аудиозахвата и кодирования: src/audio/wasapi_loopback.rs, src/audio/aac_mft.rs.
- [x] Модуль быстрого MP4-муксера: src/muxer/mp4_sink.rs.
- [x] Модуль UI-темы Material Design 3: src/ui/theme.rs.
- [x] Модуль разделяемого состояния UI: src/ui/state.rs.
- [x] Модуль системного трея Windows: src/ui/tray.rs.
- [x] Модуль оверлея на базе egui/eframe: src/ui/app.rs (OmniClipApp).
- [x] Интеграция UI в пайплайн приложения: src/main.rs (захват в omniclip-capture, UI на главном потоке).

---

## 4. Следующие шаги (Next Steps)
1. Отображение аппаратного курсора (Hardware Cursor Rendering).
2. Конфигурационный файл (omniclip.toml).
