use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;

use crate::buffer::chunk::VideoChunk;
use crate::capture::CapturedFrame;
use crate::encoder::mf_runtime::DxgiDeviceManager;

/// Packs two 32-bit unsigned integers into a 64-bit integer for Media Foundation attributes.
#[inline]
pub fn pack_u64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | (lo as u64)
}

/// Hardware-accelerated Media Foundation H.264 MFT encoder.
///
/// Hardware MFTs are always async: ProcessInput is only allowed after receiving
/// METransformNeedInput events. `need_input_credits` tracks how many inputs the
/// encoder currently accepts before the next poll cycle.
pub struct H264Encoder {
    transform: IMFTransform,
    event_generator: IMFMediaEventGenerator,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    gop_size: u32,
    stream_info: MFT_OUTPUT_STREAM_INFO,
    provides_samples: bool,
    /// Number of ProcessInput calls the async MFT currently accepts.
    need_input_credits: u32,
    codec: crate::buffer::chunk::VideoCodec,
}

impl H264Encoder {
    /// Initializes a new hardware video encoder transform with the given parameters and DXGI device manager.
    pub fn new(
        dxgi_device_mgr: &DxgiDeviceManager,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        gop_size: u32,
        codec: crate::buffer::chunk::VideoCodec,
    ) -> Result<Self> {
        let requested_subtype = match codec {
            crate::buffer::chunk::VideoCodec::H264 => MFVideoFormat_H264,
            crate::buffer::chunk::VideoCodec::Hevc => MFVideoFormat_HEVC,
        };

        let mut actual_codec = codec;
        let mut out_type_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: requested_subtype,
        };

        let flags = MFT_ENUM_FLAG_HARDWARE
            | MFT_ENUM_FLAG_SORTANDFILTER
            | MFT_ENUM_FLAG_LOCALMFT
            | MFT_ENUM_FLAG_TRANSCODE_ONLY;

        let mut activates_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut num_activates = 0u32;

        let enum_res = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&out_type_info),
                &mut activates_ptr,
                &mut num_activates,
            )
        };

        if enum_res.is_err() || num_activates == 0 {
            if codec == crate::buffer::chunk::VideoCodec::Hevc {
                log::warn!("Hardware HEVC MFT encoder not found, falling back to H.264");
                actual_codec = crate::buffer::chunk::VideoCodec::H264;
                out_type_info.guidSubtype = MFVideoFormat_H264;
                let _ = unsafe {
                    MFTEnumEx(
                        MFT_CATEGORY_VIDEO_ENCODER,
                        flags,
                        None,
                        Some(&out_type_info),
                        &mut activates_ptr,
                        &mut num_activates,
                    )
                };
            }
        }

        if num_activates == 0 || activates_ptr.is_null() {
            // Fallback to all encoders
            unsafe {
                MFTEnumEx(
                    MFT_CATEGORY_VIDEO_ENCODER,
                    MFT_ENUM_FLAG_ALL,
                    None,
                    Some(&out_type_info),
                    &mut activates_ptr,
                    &mut num_activates,
                )
                .map_err(|e| anyhow!("Failed to enumerate any video MFT encoder: {}", e))?;
            }
        }

        if num_activates == 0 || activates_ptr.is_null() {
            return Err(anyhow!("No video MFT encoder found on system"));
        }

        let act_slice = unsafe { std::slice::from_raw_parts(activates_ptr, num_activates as usize) };
        let activate = act_slice[0]
            .as_ref()
            .ok_or_else(|| anyhow!("MFT activate pointer is null"))?;

        let transform: IMFTransform = unsafe {
            activate
                .ActivateObject()
                .map_err(|e| anyhow!("Failed to activate video transform: {}", e))?
        };

        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(activates_ptr as *const _));
        }

        // Unlock async processing BEFORE SetD3DManager — hardware MFTs require this.
        unsafe {
            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
            }
            if let Ok(codec_api) = transform.cast::<windows::Win32::Media::MediaFoundation::ICodecAPI>() {
                let var = windows::core::VARIANT::from(gop_size);
                let _ = codec_api.SetValue(&windows::Win32::Media::MediaFoundation::CODECAPI_AVEncMPVGOPSize, &var);
            }
        }

        // Bind Direct3D 11 DXGI Device Manager for hardware acceleration
        let raw_mgr = dxgi_device_mgr.as_raw_param();
        if raw_mgr != 0 {
            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw_mgr)
                    .map_err(|e| anyhow!("Failed to set D3D manager on MFT: {}", e))?;
            }
        }

        let actual_subtype = match actual_codec {
            crate::buffer::chunk::VideoCodec::H264 => MFVideoFormat_H264,
            crate::buffer::chunk::VideoCodec::Hevc => MFVideoFormat_HEVC,
        };

        // Configure Output Media Type (H.264 / HEVC)
        unsafe {
            let out_type: IMFMediaType = MFCreateMediaType()?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out_type.SetGUID(&MF_MT_SUBTYPE, &actual_subtype)?;
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
            out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
            out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
            out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;

            transform
                .SetOutputType(0, &out_type, 0)
                .map_err(|e| anyhow!("Failed to set MFT output type: {}", e))?;
        }

        // Configure Input Media Type (ARGB32 from desktop duplication)
        unsafe {
            let in_type: IMFMediaType = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
            in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
            in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
            in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;

            transform
                .SetInputType(0, &in_type, 0)
                .map_err(|e| anyhow!("Failed to set MFT input type (ARGB32): {}", e))?;
        }

        // Notify stream initialization
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        let event_generator: IMFMediaEventGenerator = transform
            .cast()
            .map_err(|e| anyhow!("Transform does not implement IMFMediaEventGenerator: {}", e))?;

        let stream_info = unsafe {
            transform
                .GetOutputStreamInfo(0)
                .map_err(|e| anyhow!("Failed to get MFT output stream info: {}", e))?
        };

        let provides_samples =
            (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        info!(
            "Initialized Hardware H.264 MFT Encoder ({}x{} @ {} fps, {} bps, GOP: {})",
            width, height, fps, bitrate, gop_size
        );

        Ok(Self {
            transform,
            event_generator,
            width,
            height,
            fps,
            bitrate,
            gop_size,
            codec: actual_codec,
            stream_info,
            provides_samples,
            need_input_credits: 0,
        })
    }

    /// Returns the active compression codec (H.264 or HEVC).
    #[inline]
    pub fn codec(&self) -> crate::buffer::chunk::VideoCodec {
        self.codec
    }

    /// Encoded frame width in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Encoded frame height in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Encoded frame rate in FPS.
    #[inline]
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// Target bitrate in bits per second.
    #[inline]
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Dynamically sets the video encoding bitrate in bits per second.
    pub fn set_bitrate(&mut self, bitrate: u32) {
        self.bitrate = bitrate;
        unsafe {
            if let Ok(codec_api) = self.transform.cast::<windows::Win32::Media::MediaFoundation::ICodecAPI>() {
                let var = windows::core::VARIANT::from(bitrate);
                let _ = codec_api.SetValue(
                    &windows::Win32::Media::MediaFoundation::CODECAPI_AVEncCommonMeanBitRate,
                    &var,
                );
            }
        }
        self.request_keyframe();
    }

    /// GOP size (keyframe interval in frames).
    #[inline]
    pub fn gop_size(&self) -> u32 {
        self.gop_size
    }

    /// Requests the hardware encoder to generate an IDR keyframe on the next submitted frame.
    pub fn request_keyframe(&self) {
        if let Ok(codec_api) = self.transform.cast::<windows::Win32::Media::MediaFoundation::ICodecAPI>() {
            let var = windows::core::VARIANT::from(1u32);
            unsafe {
                let _ = codec_api.SetValue(
                    &windows::Win32::Media::MediaFoundation::CODECAPI_AVEncVideoForceKeyFrame,
                    &var,
                );
            }
            debug!("Forced IDR keyframe via ICodecAPI");
        }
    }

    /// Returns true if the encoder currently has a credit to accept a new frame.
    #[inline]
    pub fn can_feed(&self) -> bool {
        self.need_input_credits > 0
    }

    /// Feeds a captured Direct3D 11 texture into the encoder with zero-copy VRAM surface buffer.
    ///
    /// Only call this when `can_feed()` returns true (i.e. after draining events via `poll_output`).
    pub fn feed_frame(&mut self, frame: &CapturedFrame, duration_hns: i64) -> Result<()> {
        if self.need_input_credits == 0 {
            // Encoder not ready yet; skip silently to avoid MF_E_NOTACCEPTING.
            return Ok(());
        }

        let media_buffer: IMFMediaBuffer = unsafe {
            MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &frame.texture, 0, false)
                .map_err(|e| anyhow!("Failed to create DXGI surface buffer: {}", e))?
        };

        let sample: IMFSample = unsafe {
            MFCreateSample().map_err(|e| anyhow!("Failed to create MF sample: {}", e))?
        };

        unsafe {
            sample
                .AddBuffer(&media_buffer)
                .map_err(|e| anyhow!("Failed to add buffer to sample: {}", e))?;
            sample
                .SetSampleTime(frame.hns_timestamp)
                .map_err(|e| anyhow!("Failed to set sample time: {}", e))?;
            sample
                .SetSampleDuration(duration_hns)
                .map_err(|e| anyhow!("Failed to set sample duration: {}", e))?;

            self.transform
                .ProcessInput(0, &sample, 0)
                .map_err(|e| anyhow!("MFT ProcessInput failed: {}", e))?;
        }

        self.need_input_credits -= 1;
        Ok(())
    }

    /// Drains the async MFT event queue (non-blocking) and collects encoded video chunks.
    ///
    /// Each `METransformNeedInput` event increments the internal credit counter, allowing
    /// the next `feed_frame` call. Each `METransformHaveOutput` event triggers a `ProcessOutput`
    /// to collect encoded H.264 data.
    pub fn poll_output(&mut self) -> Result<Vec<VideoChunk>> {
        let mut chunks = Vec::new();

        loop {
            // MF_EVENT_FLAG_NO_WAIT (= 1): return immediately if no event is pending.
            let event = match unsafe {
                self.event_generator
                    .GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(1))
            } {
                Ok(ev) => ev,
                Err(_) => break, // No more events right now — exit cleanly.
            };

            let event_type = match unsafe { event.GetType() } {
                Ok(t) => t,
                Err(_) => continue,
            };

            if event_type == METransformNeedInput.0 as u32 {
                // Encoder slot freed: we may submit one more input frame.
                self.need_input_credits += 1;
            } else if event_type == METransformHaveOutput.0 as u32 {
                // Encoder produced output: collect it.
                let mut out_sample: Option<IMFSample> = None;
                if !self.provides_samples {
                    let s = unsafe { MFCreateSample()? };
                    let buf = unsafe { MFCreateMemoryBuffer(self.stream_info.cbSize.max(65536))? };
                    unsafe { s.AddBuffer(&buf)? };
                    out_sample = Some(s);
                }

                let out_buffer = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: match &out_sample {
                        Some(s) => unsafe { std::mem::transmute_copy(s) },
                        None => unsafe { std::mem::zeroed() },
                    },
                    dwStatus: 0,
                    pEvents: unsafe { std::mem::zeroed() },
                };

                let mut output_buffers = [out_buffer];
                let mut status = 0u32;

                match unsafe { self.transform.ProcessOutput(0, &mut output_buffers, &mut status) } {
                    Ok(()) => {
                        let sample_opt = if self.provides_samples {
                            output_buffers[0].pSample.as_ref().cloned()
                        } else {
                            out_sample
                        };
                        if let Some(s) = sample_opt {
                            if let Some(chunk) = Self::sample_to_chunk(&s)? {
                                chunks.push(chunk);
                            }
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        debug!("MFT stream change: re-applying output media type");
                        let new_type = unsafe { self.transform.GetOutputAvailableType(0, 0)? };
                        unsafe { self.transform.SetOutputType(0, &new_type, 0)? };
                    }
                    Err(e) => {
                        warn!("ProcessOutput error: {}", e);
                    }
                }
            }
            // Ignore other event types (METransformDrainComplete, etc.)
        }

        Ok(chunks)
    }


    /// Converts an encoded IMFSample to a VideoChunk with PTS, duration, and keyframe detection.
    fn sample_to_chunk(sample: &IMFSample) -> Result<Option<VideoChunk>> {
        let pts = unsafe { sample.GetSampleTime().unwrap_or(0) };
        let duration = unsafe { sample.GetSampleDuration().unwrap_or(166_666) };

        // Check MFSampleExtension_CleanPoint attribute (keyframe)
        let is_clean_point = unsafe {
            sample
                .GetUINT32(&MFSampleExtension_CleanPoint)
                .map(|val| val != 0)
                .unwrap_or(false)
        };

        let buffer = unsafe {
            sample
                .ConvertToContiguousBuffer()
                .map_err(|e| anyhow!("Failed to convert sample to contiguous buffer: {}", e))?
        };

        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut cur_len = 0u32;
        let mut max_len = 0u32;

        unsafe {
            buffer
                .Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
                .map_err(|e| anyhow!("Failed to lock media buffer: {}", e))?;
        }

        if ptr.is_null() || cur_len == 0 {
            unsafe {
                let _ = buffer.Unlock();
            }
            return Ok(None);
        }

        let slice = unsafe { std::slice::from_raw_parts(ptr, cur_len as usize) };
        let is_idr_nalu = Self::has_idr_nalu(slice);
        let is_keyframe = is_clean_point || is_idr_nalu;

        let data = slice.to_vec();

        unsafe {
            let _ = buffer.Unlock();
        }

        Ok(Some(VideoChunk {
            is_keyframe,
            pts_hns: pts,
            dts_hns: pts,
            duration_hns: duration,
            data,
        }))
    }

    /// Scans H.264 byte stream for IDR NAL units (nal_unit_type == 5 or 7 SPS).
    fn has_idr_nalu(data: &[u8]) -> bool {
        let mut i = 0;
        while i + 4 < data.len() {
            // Check 4-byte start code 0x00 0x00 0x00 0x01
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
                let nal_type = data[i + 4] & 0x1F;
                if nal_type == 5 || nal_type == 7 {
                    return true;
                }
                i += 4;
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                // Check 3-byte start code 0x00 0x00 0x01
                let nal_type = data[i + 3] & 0x1F;
                if nal_type == 5 || nal_type == 7 {
                    return true;
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        false
    }

    /// Flushes the encoder pipeline.
    pub fn flush(&mut self) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .map_err(|e| anyhow!("Failed to flush MFT encoder: {}", e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::D3D11Context;
    use crate::encoder::mf_runtime::MfPlatformGuard;

    #[test]
    fn test_encoder_async_flow() {
        let _guard = MfPlatformGuard::init().expect("MF init failed");
        let d3d = D3D11Context::new().expect("D3D11 init failed");
        let dev_mgr = DxgiDeviceManager::new(d3d.device()).expect("DxgiDeviceManager failed");
        let mut enc = H264Encoder::new(&dev_mgr, 1920, 1080, 60, 20_000_000, 60, crate::buffer::chunk::VideoCodec::H264).expect("Encoder init failed");

        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::Common::*;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 1920,
            Height: 1080,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe {
            d3d.device().CreateTexture2D(&desc, None, Some(&mut tex)).expect("CreateTexture2D failed");
        }
        let base_frame = crate::capture::CapturedFrame {
            texture: tex.unwrap(),
            qpc_timestamp: 0,
            hns_timestamp: 0,
            width: 1920,
            height: 1080,
        };

        let mut total_chunks = 0usize;
        for i in 0..30u64 {
            // 1. Drain events first — this gives us NeedInput credits and collects output.
            let chunks = enc.poll_output().expect("poll_output failed");
            total_chunks += chunks.len();

            // 2. Feed only when encoder is ready.
            if enc.can_feed() {
                let mut f = base_frame.clone();
                f.hns_timestamp = i as i64 * 166_666;
                enc.feed_frame(&f, 166_666).expect("feed_frame failed");
                println!("Frame {}: fed, credits_remaining={}, chunks_so_far={}", i, enc.need_input_credits, total_chunks);
            } else {
                println!("Frame {}: no credit yet, skipping", i);
            }
        }
        // Drain remaining output
        let final_chunks = enc.poll_output().expect("final poll failed");
        total_chunks += final_chunks.len();
        println!("Total encoded chunks: {}", total_chunks);
        assert!(total_chunks > 0, "Expected at least one encoded chunk after 30 frames");
    }
}
