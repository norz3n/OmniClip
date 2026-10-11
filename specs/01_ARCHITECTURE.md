# 01. Архитектура и модель данных

## 1. Концепция конвейера
Ключевая цель — нулевая нагрузка на CPU и шину PCIe при непрерывной фоновой записи игрового процесса в разрешении до 4K при 60/120 FPS.

```
       [GPU Video Memory]                             [System RAM]
+-------------------------------+             +---------------------------+
| Direct3D 11 Render Target     |             |                           |
|              │                |             |                           |
|       (Desktop Duplication)   |             |                           |
|              ▼                |             |                           |
|    ID3D11Texture2D (VRAM)     |             |                           |
|              │                |             |                           |
|       (Zero-Copy Share)       |             |                           |
|              ▼                |             |                           |
| Hardware MFT (NVENC/AMF/QSV)  | ──(NALUs)─> | [Circular RAM Buffer]     |
+-------------------------------+             |  - 60-120 сек сжатых GOP  |
                                              |  - IDR Checkpoints        |
                                              |  - WASAPI AAC Samples     |
                                              +-------------┬-------------+
                                                            │
                                              [Hotkey Trigger (Alt+F10)]
                                                            │
                                                            ▼
                                              [Fast Streaming MP4 Sink]
                                              (Сброс на диск за < 300 мс)
```

## 2. Модель потоков (Threading Model)

1. **Capture & Encoder Thread**:
   - Работает на частоте обновления экрана (напр. 60 Гц).
   - Вызывает `AcquireNextFrame`.
   - Передает захваченную `ID3D11Texture2D` в `IMFTransform::ProcessInput`.
   - Забирает сжатые сэмплы через `IMFTransform::ProcessOutput`.
   - Отправляет готовые сжатые `EncodedChunk` в канал передачи кольцевого буфера.

2. **Audio Capture Thread**:
   - Читает буфер WASAPI Loopback (`IAudioCaptureClient::GetBuffer`).
   - Передаёт PCM-сэмплы в AAC MFT энкодер.
   - Отправляет сжатые аудио-чанки в канал кольцевого буфера.

3. **Ring Buffer Coordinator**:
   - Принимает пакеты видео и аудио из каналов (`crossbeam_channel::Receiver`).
   - Сохраняет пакеты в кольцевую память (`VecDeque`).
   - Контролирует суммарную длительность буфера и выравнивает начало буфера по ключевым кадрам (IDR).

4. **Hotkey Listener Thread**:
   - Создаёт невидимое окно сообщений (`HWND_MESSAGE`) или устанавливает глобальный `RegisterHotKey`.
   - Перехватывает комбинацию сохранения (по умолчанию `Alt + F10`).
   - При срабатывании отправляет сигнал `DumpReplay` в рабочий поток сохранения.

5. **Disk Writer Thread (Worker)**:
   - При получении сигнала `DumpReplay` атомарно забирает снимок кольцевого буфера.
   - Стримит чанки в MP4-контейнер (`IMFSinkWriter` или встроенный MP4-муксер).
   - Не прерывает и не блокирует основной цикл захвата.

## 3. Временная шкала и синхронизация (PTS / QPC)

* Все отметки времени рассчитываются на основе Windows **QueryPerformanceCounter (QPC)**:
  $$\text{Timestamp}_{\text{HNS}} = \frac{\text{QPC} \times 10\,000\,000}{\text{QPC\_Frequency}}$$
* Формат таймстемпов Media Foundation — **HNS (Hundred-Nanosecond units, 100 нс = $10^{-7}$ сек)**.
* Аудио и видео синхронизируются по абсолютным HNS-меткам от одного базового QPC-старта.
