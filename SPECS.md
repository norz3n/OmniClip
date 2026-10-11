# OmniClip — Архитектурная и техническая спецификация (Master Specs)

Сверхлёгкая система мгновенных повторов (Instant Replay / RePlay buffer) на Rust для Windows (10/11), работающая по принципу аппаратного захвата и кодирования в реальном времени с минимальным потреблением ресурсов (< 1% CPU, Zero-Copy GPU).

---

## 1. Концепция и ключевые требования

* **Платформа**: Windows 10 (1903+) / Windows 11, x86_64.
* **Язык**: Rust (2021 edition), Win32 API через официальный крейт `windows`.
* **Zero-Copy пайплайн**: Захваченный кадр не считывается в системную память CPU (RAM). Он передаётся из DXGI Desktop Duplication напрямую в аппаратный энкодер через Direct3D 11 Device Manager в VRAM.
* **Аппаратное кодирование**: Windows Media Foundation Hardware MFT (NVIDIA NVENC, AMD AMF, Intel QSV). Отсутствие сторонних тяжеловесных библиотек (FFmpeg) и закрытых проприетарных SDK поставщиков GPU.
* **Кольцевой буфер (Ring Buffer)**: Хранит сжатые пакеты (H.264 NALU / AAC), а не сырые кадры. 60 секунд записи при битрейте 30 Мбит/с занимают ~225 МБ RAM.
* **Целостность потока (IDR Alignment)**: Обрезка старых данных происходит строго по границам ключевых кадров (IDR/SPS/PPS), гарантируя мгновенное воспроизведение без артефактов.
* **Мгновенный сброс (Dump)**: При нажатии хоткея (напр. `Alt+F10`) данные из кольцевого буфера без перекодирования упаковываются в `.mp4` за < 300 мс.

---

## 2. Архитектура потоков и данных (Data Flow)

```
[Экран / Desktop] 
       │ 
       ▼ (GPU VRAM)
[Capture Thread] (DXGI Output Duplication)
  - IDXGIOutputDuplication::AcquireNextFrame
  - ID3D11Texture2D (B8G8R8A8_UNORM)
  - Отметка таймстемпа QPC (QueryPerformanceCounter)
       │ 
       ▼ (Direct3D 11 Shared Texture Handle / Device Context)
[Encoder Thread] (Windows Media Foundation Hardware MFT)
  - IMFDXGIDeviceManager (Zero-Copy)
  - H.264 / HEVC Hardware MFT (NVENC / AMF / QSV)
  - Получение IMFSample (NAL-юниты с PTS/DTS)
       │
       ▼ (Crossbeam Channel / MPSC)
[Ring Buffer Manager] (RAM)
  - Video Ring Buffer (Хранение EncodedChunk, IDR-индексация)
  - Audio Ring Buffer (Хранение AACChunk)
       ▲
       │ (Crossbeam Channel / MPSC)
[Audio Thread] (WASAPI Loopback)
  - IAudioClient (AUDCLNT_STREAMFLAGS_LOOPBACK)
  - Захват PCM 48kHz Stereo
  - Сжатие в AAC через Media Foundation AAC MFT
       ▲
       │
[Hotkey Listener Thread] (Win32 RegisterHotKey / Low-level hook)
  - Перехват хоткея (Alt+F10)
  - Сигнал триггера записи
       │
       ▼
[Disk Writer Thread] (Fast MP4 Muxer)
  - Снимок кольцевого буфера
  - Запись ftyp/moov/mdat в MP4 без перекодирования
```

---

## 3. Модульная структура приложения

```
src/
├── main.rs                   # Инициализация COM/MF, координация потоков, graceful shutdown
├── config.rs                 # Настройки (длина буфера, битрейт, FPS, хоткеи, путь сохранения)
├── capture/                  # Подсистема захвата
│   ├── mod.rs                # Трейт FrameCapture
│   ├── d3d11_device.rs       # Инициализация ID3D11Device, ID3D11DeviceContext, DXGI Adapter
│   └── dxgi_duplicator.rs    # Реализация IDXGIOutputDuplication, обработка смены разрешений
├── encoder/                  # Подсистема аппаратного кодирования
│   ├── mod.rs                # Трейт VideoEncoder
│   ├── mf_runtime.rs         # MFStartup, IMFDXGIDeviceManager
│   └── h264_mft.rs           # Настройка IMFTransform (H.264), управление параметрами CBR/VBR, GOP
├── audio/                    # Подсистема аудиозахвата и кодирования
│   ├── mod.rs                # Трейт AudioCapture
│   ├── wasapi_loopback.rs    # Захват аудио с дефолтного устройства вывода (Speakers/Headphones)
│   └── aac_mft.rs            # MFT AAC энкодер
├── buffer/                   # Кольцевой буфер
│   ├── mod.rs
│   ├── chunk.rs              # Структуры EncodedChunk, VideoSample, AudioSample
│   └── ring_buffer.rs        # Потокобезопасный кольцевой буфер с IDR-выравниванием
├── muxer/                    # Упаковщик в MP4
│   ├── mod.rs
│   └── mp4_sink.rs           # Запись видео/аудио потоков в контейнер MP4
└── platform/                 # Платформозависимый функционал Windows
    ├── mod.rs
    ├── hotkey.rs             # Регистрация и перехват глобальных хоткеев (RegisterHotKey)
    └── time.rs               # Высокоточный таймер QPC (QueryPerformanceCounter)
```

---

## 4. Спецификация компонентов

### 4.1. Capture Module (`capture/`)
1. **API**: `IDXGIOutput1::DuplicateOutput` -> `IDXGIOutputDuplication`.
2. **Формат**: `DXGI_FORMAT_B8G8R8A8_UNORM`.
3. **Обработка событий**:
   - `AcquireNextFrame(timeout_ms)`: при таймауте (нет изменений на экране) дублировать предыдущий кадр с новым PTS или отдавать сигнал энкодеру в зависимости от выбранного режима VFR/CFR.
   - `DXGI_ERROR_ACCESS_LOST`: пересоздание дубликатора (смена разрешения, смена режима монитора, UAC screen).
   - `DXGI_ERROR_DEVICE_REMOVED`: полное пересоздание Direct3D 11 устройства.

### 4.2. Encoder Module (`encoder/`)
1. **API**: Windows Media Foundation (`MFT_CATEGORY_VIDEO_ENCODER`, `CLSID_CMSH264EncoderMFT` или поиск аппаратного энкодера через `MFTEnumEx`).
2. **Атрибуты аппаратного ускорения**:
   - `MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS = TRUE`
   - `MFT_MESSAGE_SET_D3D_MANAGER` с указанием `IMFDXGIDeviceManager`.
3. **Параметры сжатия**:
   - Профиль: High Profile (`eAVEncH264VProfile_High`).
   - Управление битрейтом: `CODECAPI_AVEncCommonRateControlMode` = CBR / VBR (30–50 Mbps для 1080p/1440p).
   - GOP Size: Фиксированный интервал ключевых кадров (напр. 60 кадров = 1 сек при 60 FPS или 120 кадров = 2 сек). Это гарантирует шаг обрезки буфера не более 1–2 секунд.
   - Low Latency: `CODECAPI_AVLowLatencyMode = TRUE`.

### 4.3. Circular Buffer Module (`buffer/`)
1. **Модель данных**:
   ```rust
   pub struct EncodedVideoChunk {
       pub is_keyframe: bool,
       pub pts: i64,          // В 100-наносекундных интервалах (MF HNS)
       pub dts: i64,
       pub duration: i64,
       pub data: Vec<u8>,     // Сжатые NAL-юниты (Annex B или AVCC)
   }
   ```
2. **Алгоритм очистки**:
   - Буфер поддерживает список индексов ключевых кадров (`keyframe_indices: VecDeque<usize>`).
   - Если общая длительность (`last_pts - first_pts`) превышает `max_duration_secs`:
     - Находится первый ключевой кадр, удаление до которого оставляет не менее `max_duration_secs` данных.
     - Все пакеты до этого ключевого кадра отсекаются.
     - **Инвариант**: Первый видеопакет в буфере — ВСЕГДА ключевой кадр (IDR с SPS/PPS).

### 4.4. Audio Module (`audio/`)
1. **API**: WASAPI Loopback (`IMMDeviceEnumerator` -> `eRender` -> `IAudioClient`).
2. **Формат захвата**: `WAVEFORMATEX` (обычно 48 kHz, 32-bit float, Stereo).
3. **Кодирование**: Media Foundation AAC Encoder -> `48 kHz, 192–320 kbps Stereo`.
4. **Синхронизация**: Каждый аудиопакет получает PTS, привязанный к тому же базовому значению `QueryPerformanceCounter`, что и видеокадры.

### 4.5. Fast MP4 Muxer (`muxer/`)
1. **Механизм**: Прямая запись MP4 атомов (`ftyp`, `moov`, `mdat`) без повторного перекодирования.
2. **Атомы**:
   - `ftyp`: сигнатура `isom`/`mp42`.
   - `moov`: содержит `trak` для видео и аудио, таблицы сэмплов `stts`, `stss` (ключевые кадры), `stsz` (размеры сэмплов), `stco` (смещения).
   - FastStart: размещение `moov` в начале файла для моментального открытия видеоплеерами.

---

## 5. Зависимости (`Cargo.toml`)

```toml
[package]
name = "omni_clip"
version = "0.1.0"
edition = "2021"

[dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Media_MediaFoundation",
    "Win32_Media_Audio",
    "Win32_System_Com",
    "Win32_System_Performance",
    "Win32_UI_WindowsAndMessaging",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_Security"
] }
crossbeam-channel = "0.5"
parking_lot = "0.12"
anyhow = "1.0"
thiserror = "2.0"
log = "0.4"
env_logger = "0.11"
```

---

## 6. Метрики производительности и бюджет ресурсов

| Метрика | Бюджет | Механизм контроля |
| :--- | :--- | :--- |
| **CPU Usage** | < 1.0% | Zero-Copy: текстуры остаются в VRAM, кодирование выполняет специализированный ASIC GPU (NVENC/AMF) |
| **RAM Usage** | 200–400 МБ | Хранятся только H.264/AAC сэмплы (60 сек при 35 Мбит/с = ~260 МБ) |
| **GPU Engine Usage** | < 2% 3D / ~25% Video Encode | Видеочип задействует выделенный аппаратный энкодер, не влияя на 3D рендеринг игры |
| **Время сброса клипа** | < 300 мс | Потоковая запись сырых пакетов в файл на NVMe/SSD |
