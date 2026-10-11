# 05. Аудиозахват (WASAPI Loopback) и AAC

## 1. Обзор технологии
Для захвата чистого звука игры и операционной системы используется **Windows Audio Session API (WASAPI)** в режиме **Loopback Capture**.

Преимущества:
- Захват звука с дефолтного устройства вывода (Speakers/Headphones) без виртуальных аудиокабелей.
- Минимальная задержка аудиопакетов (~10 мс).
- Исходный формат: 32-битный IEEE Float, 48 000 Гц, Стерео.

## 2. Инициализация WASAPI Loopback

1. Получение дефолтного аудиоустройства:
   ```rust
   use windows::Win32::Media::Audio::{
       MMDeviceEnumerator, IMMDeviceEnumerator, eRender, eConsole,
       IAudioClient, IAudioCaptureClient, AUDCLNT_STREAMFLAGS_LOOPBACK,
       AUDCLNT_SHAREMODE_SHARED
   };

   let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
   let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
   let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
   ```

2. Получение формата и инициализация:
   ```rust
   let mix_format = audio_client.GetMixFormat()?;
   // 1 секунда буфера в 100-нс интервалах
   let buffer_duration_hns: i64 = 10_000_000;

   audio_client.Initialize(
       AUDCLNT_SHAREMODE_SHARED,
       AUDCLNT_STREAMFLAGS_LOOPBACK,
       buffer_duration_hns,
       0,
       mix_format,
       None
   )?;

   let capture_client: IAudioCaptureClient = audio_client.GetService()?;
   audio_client.Start()?;
   ```

## 3. Чтение буфера и обработка тишины

В цикле аудиопотока:
```rust
let mut packet_length = capture_client.GetNextPacketSize()?;
while packet_length > 0 {
    let mut data_ptr: *mut u8 = std::ptr::null_mut();
    let mut num_frames = 0u32;
    let mut flags = 0u32;
    let mut dev_position = 0u64;
    let mut qpc_position = 0u64;

    capture_client.GetBuffer(
        &mut data_ptr,
        &mut num_frames,
        &mut flags,
        Some(&mut dev_position),
        Some(&mut qpc_position)
    )?;

    // Если AUDCLNT_BUFFERFLAGS_SILENT — устройство отдаёт тишину, заполняем нулями.
    // Преобразуем PCM Float32 в сэмплы для AAC-энкодера.

    capture_client.ReleaseBuffer(num_frames)?;
    packet_length = capture_client.GetNextPacketSize()?;
}
```

## 4. Кодирование в AAC через Media Foundation MFT

1. Создание `CLSID_AACMFTEncoder`.
2. Входной формат: `MFAudioFormat_Float` или `MFAudioFormat_PCM`, 48000 Hz, 2 channels.
3. Выходной формат: `MFAudioFormat_AAC`, 48000 Hz, 2 channels, 192–320 kbps.
4. Выходные AAC-пакеты получают PTS на базе `qpc_position` для абсолютной синхронизации с видео.
