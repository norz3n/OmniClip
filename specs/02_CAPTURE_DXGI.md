# 02. Захват экрана (DXGI Desktop Duplication)

## 1. Обзор технологии
Модуль захвата построен на базе **DirectX Graphics Infrastructure (DXGI) Desktop Duplication API** (`IDXGIOutputDuplication`).

Преимущества:
- Прямой доступ к кадрам GPU Desktop Window Manager (DWM).
- Текстура создаётся на GPU (`ID3D11Texture2D`), не требует копирования в CPU RAM (Zero-Copy).
- Минимальная задержка (менее 1 мс на захват кадра).

## 2. Инициализация Direct3D 11 устройства

```rust
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT
};

// Флаги создания: обязательны BGRA и VIDEO_SUPPORT для взаимодействия с Media Foundation
let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
let mut device: Option<ID3D11Device> = None;
let mut context: Option<ID3D11DeviceContext> = None;
```

## 3. Подключение к дисплею и дубликация

1. Перечисление адаптеров через `IDXGIFactory1::EnumAdapters1`.
2. Получение `IDXGIOutput` основного монитора:
   ```rust
   let output: IDXGIOutput = adapter.EnumOutputs(0)?;
   let output1: IDXGIOutput1 = output.cast()?;
   let duplicator: IDXGIOutputDuplication = output1.DuplicateOutput(&device)?;
   ```
3. Получение метаданных дисплея (`DXGI_OUTDUPL_DESC`):
   - Ширина, высота, частота обновления (Refresh Rate).
   - Формат кадра: обычно `DXGI_FORMAT_B8G8R8A8_UNORM`.

## 4. Цикл захвата кадра

Каждый кадр извлекается через `AcquireNextFrame`:

```rust
let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
let mut desktop_resource: Option<IDXGIResource> = None;

match duplicator.AcquireNextFrame(timeout_ms, &mut frame_info, &mut desktop_resource) {
    Ok(()) => {
        if frame_info.AccumulatedFrames > 0 {
            let resource = desktop_resource.unwrap();
            let texture: ID3D11Texture2D = resource.cast()?;
            // Передача текстуры в энкодер
        }
        duplicator.ReleaseFrame()?;
    }
    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
        // Кадр на экране не изменился. 
        // Повторяем предыдущий кадр или пропускаем тик в зависимости от CFR/VFR режима.
    }
    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
        // Произошла смена видеорежима, разрешения или переключение рабочего стола.
        // Необходим рестарт дубликатора.
    }
    Err(e) => return Err(e.into()),
}
```

## 5. Обработка граничных ситуаций

* **`DXGI_ERROR_ACCESS_LOST`**: Пользователь нажал `Ctrl+Alt+Del`, сменил разрешение экрана или игра вошла в Fullscreen Exclusive.
  * *Решение*: Вызвать пересоздание `DuplicateOutput`. Если игра блокирует захват, временно ожидать восстановления DWM.
* **`DXGI_ERROR_DEVICE_REMOVED`**: Сбой видеодрайвера или смена активного GPU.
  * *Решение*: Полное пересоздание `ID3D11Device`, `ID3D11DeviceContext`, `IMFDXGIDeviceManager` и энкодера.
* **Мышь (Hardware Cursor)**: Если курсор аппаратный и не рисуется в текстуру, `DXGI_OUTDUPL_POINTER_SHAPE_INFO` и `PointerPosition` позволяют программно отрисовать курсор в текстуру через Direct2D или шейдер (опционально, настраиваемо).
