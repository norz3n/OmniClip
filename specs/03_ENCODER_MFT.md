# 03. Аппаратное кодирование (Windows Media Foundation Hardware MFT)

## 1. Обзор технологии
Для аппаратного кодирования в Windows используется **Media Foundation Transform (MFT)** со статусом аппаратного ускорения (`MFT_ENUM_FLAG_HARDWARE`).

Преимущества:
- Унифицированный интерфейс для NVIDIA NVENC, AMD AMF и Intel QuickSync.
- Прямое связывание с Direct3D 11 через `IMFDXGIDeviceManager`.
- Текстура передается в энкодер в VRAM: CPU загружен на **0.1–0.5%**.

## 2. Инициализация и Device Manager

1. Запуск платформы Media Foundation:
   ```rust
   use windows::Win32::Media::MediaFoundation::{MFStartup, MF_VERSION, MFSTARTUP_NOSOCKET};
   unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?; }
   ```

2. Создание менеджера устройств DXGI:
   ```rust
   use windows::Win32::Media::MediaFoundation::{MFCreateDXGIDeviceManager, IMFDXGIDeviceManager};
   let mut reset_token = 0u32;
   let mut device_manager: Option<IMFDXGIDeviceManager> = None;
   unsafe {
       MFCreateDXGIDeviceManager(&mut reset_token, &mut device_manager)?;
       device_manager.as_ref().unwrap().ResetDevice(&d3d11_device, reset_token)?;
   }
   ```

## 3. Выбор и активация аппаратного H.264/HEVC MFT

1. Поиск аппаратного энкодера через `MFTEnumEx`:
   - Категория: `MFT_CATEGORY_VIDEO_ENCODER`.
   - Флаги: `MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER`.
   - Подтип входа: `MFVideoFormat_NV12` или `MFVideoFormat_ARGB32`.
   - Подтип выхода: `MFVideoFormat_H264` (или `MFVideoFormat_HEVC`).

2. Передача `IMFDXGIDeviceManager` в найденный `IMFTransform`:
   ```rust
   let attributes = encoder_transform.GetAttributes()?;
   attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
   
   encoder_transform.ProcessMessage(
       MFT_MESSAGE_SET_D3D_MANAGER,
       device_manager.as_raw() as usize,
   )?;
   ```

## 4. Конфигурация параметров сжатия (ICodecAPI)

Энкодер настраивается через интерфейс `ICodecAPI`:
- **Битрейт**: `CODECAPI_AVEncCommonMeanBitRate` (например, 35_000_000 бит/с = 35 Мбит/с).
- **Режим управления битрейтом**: `CODECAPI_AVEncCommonRateControlMode` = `eAVEncCommonRateControlMode_CBR` или `PeakConstrainedVBR`.
- **Интервал ключевых кадров (GOP)**:
  - `CODECAPI_AVEncMPVGOPSize` = 60 (при 60 FPS = ровно 1 секунда между IDR-кадрами).
  - Критично: короткий GOP (1–2 сек) гарантирует точную и быструю обрезку буфера повторов.
- **Низкая задержка**: `CODECAPI_AVLowLatencyMode` = `TRUE`.
- **Профиль**: `eAVEncH264VProfile_High`.

## 5. Цикл обработки кадров (ProcessInput -> ProcessOutput)

1. Для каждого входящего `ID3D11Texture2D`:
   - Создаётся `IMFSample` с `IMFMediaBuffer`, обёртывающим текстуру через `MFCreateDXGISurfaceBuffer`.
   - Устанавливаются `SampleTime` (HNS PTS) и `SampleDuration`.
   - Вызывается `encoder_transform.ProcessInput(0, &sample, 0)`.

2. Чтение готовых сжатых пакетов:
   - Вызывается `encoder_transform.ProcessOutput(0, &mut output_buffers)`.
   - Извлекается выходной буфер с бинарными NAL-юнитами.
   - Проверяется флаг `MFSampleExtension_CleanPoint` (является ли кадр ключевым / IDR).
   - Чанк передаётся в кольцевой буфер.
