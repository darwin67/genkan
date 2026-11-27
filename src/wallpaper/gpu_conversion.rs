//! Exact P010 conversion for desktop movies. Other formats stay in videoconvert.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use bytes::Bytes;
use gstreamer as gst;
use gstreamer_video::{self as video, prelude::*};
use iced::futures::executor::block_on;
use iced::wgpu::{self, util::DeviceExt};

pub(super) fn configure(source: &gst::Pad, sink: &gstreamer_app::AppSink) {
    let sink = sink.clone();
    let select = move |caps: &gst::CapsRef| {
        let supported = video::VideoInfo::from_caps(caps).is_ok_and(|info| supports(&info));
        let selected = gst::Caps::builder("video/x-raw")
            .field("format", if supported { "P010_10LE" } else { "RGBA" })
            .field(
                "framerate",
                gst::Fraction::new(super::OUTPUT_FRAMES_PER_SECOND, 1),
            )
            .build();
        sink.set_caps(Some(&selected));
    };
    if let Some(caps) = source.current_caps() {
        select(&caps);
    }
    // Do not freeze the decoder's initial dimensions or color metadata. A new
    // stream may resize or need the ordinary RGBA conversion instead.
    source.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, probe| {
        if let Some(gst::PadProbeData::Event(event)) = &probe.data {
            if let gst::EventView::Caps(event) = event.view() {
                select(event.caps());
            }
        }
        gst::PadProbeReturn::Ok
    });
}

pub(super) fn supports(info: &video::VideoInfo) -> bool {
    info.format() == video::VideoFormat::P01010le
        && info.width() >= 2
        && info.width().is_multiple_of(2)
        && info.height() >= 2
        && info.height().is_multiple_of(2)
        && info.width() <= 8192
        && info.height() <= 8192
        && info.interlace_mode() == video::VideoInterlaceMode::Progressive
        && info.views() == 1
        && matches!(
            info.multiview_mode(),
            video::VideoMultiviewMode::None | video::VideoMultiviewMode::Mono
        )
        && info.colorimetry().to_string() == "2:3:7:1"
        && info.chroma_site() == video::VideoChromaSite::MPEG2
}

#[derive(Default)]
pub(super) struct Converter {
    gpu: Option<Gpu>,
    cpu: Option<video::VideoConverter>,
    disabled: bool,
    checked: bool,
}

impl Converter {
    pub fn convert(
        &mut self,
        frame: &video::VideoFrameRef<&gst::BufferRef>,
    ) -> Result<Bytes, gst::FlowError> {
        let info = frame.info();
        if self.cpu.as_ref().is_none_or(|cpu| cpu.in_info() != info) {
            let output =
                video::VideoInfo::builder(video::VideoFormat::Rgba, info.width(), info.height())
                    .fps(info.fps())
                    .colorimetry(&"sRGB".parse().unwrap())
                    .build()
                    .map_err(|_| gst::FlowError::NotNegotiated)?;
            self.cpu = Some(
                video::VideoConverter::new(info, &output, None)
                    .map_err(|_| gst::FlowError::NotNegotiated)?,
            );
            let old = self.gpu.take();
            let _ = catch_unwind(AssertUnwindSafe(|| drop(old)));
            self.checked = false;
        }
        if !self.disabled && supports(info) && !frame.is_interlaced() {
            let result = catch_unwind(AssertUnwindSafe(|| {
                if self.gpu.is_none() {
                    self.gpu = Some(Gpu::new(info, false)?);
                }
                self.gpu.as_ref().unwrap().convert(frame)
            }))
            .unwrap_or_else(|_| Err("GPU conversion panicked".into()));
            match result {
                Ok(pixels) => {
                    if self.checked {
                        return Ok(pixels);
                    }
                    // Validate the actual driver and GStreamer combination once,
                    // before publishing any GPU-converted pixels.
                    let reference = self.cpu_frame(frame)?;
                    if pixels == reference {
                        self.checked = true;
                        eprintln!("genkan wallpaper: exact GPU color conversion enabled");
                        return Ok(pixels);
                    }
                    self.disable("GPU pixels differ from videoconvert");
                    return Ok(reference);
                }
                Err(error) => self.disable(&error),
            }
        }
        self.cpu_frame(frame)
    }

    fn disable(&mut self, reason: &str) {
        self.disabled = true;
        let gpu = self.gpu.take();
        let _ = catch_unwind(AssertUnwindSafe(|| drop(gpu)));
        eprintln!("genkan wallpaper: {reason}; retaining CPU color conversion");
    }

    fn cpu_frame(
        &self,
        frame: &video::VideoFrameRef<&gst::BufferRef>,
    ) -> Result<Bytes, gst::FlowError> {
        let converter = self.cpu.as_ref().unwrap();
        let info = converter.out_info();
        let mut buffer = gst::Buffer::with_size(info.size()).map_err(|_| gst::FlowError::Error)?;
        {
            let mut output =
                video::VideoFrameRef::from_buffer_ref_writable(buffer.get_mut().unwrap(), info)
                    .map_err(|_| gst::FlowError::Error)?;
            converter.frame_ref(frame, &mut output);
        }
        let mapped = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
        Ok(Bytes::copy_from_slice(mapped.as_slice()))
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        let gpu = self.gpu.take();
        let _ = catch_unwind(AssertUnwindSafe(|| drop(gpu)));
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bindings: wgpu::BindGroup,
    y: wgpu::Buffer,
    uv: wgpu::Buffer,
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    strides: [usize; 2],
    failed: Arc<AtomicBool>,
}

impl Gpu {
    fn new(info: &video::VideoInfo, allow_software: bool) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        if !allow_software && adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Err("software Vulkan adapter".into());
        }
        let width = info.width();
        let height = info.height();
        let strides = [info.stride()[0] as usize, info.stride()[1] as usize];
        let sizes = [
            strides[0] as u64 * height as u64,
            strides[1] as u64 * (height / 2) as u64,
            width as u64 * height as u64 * 4,
        ];
        if strides
            .iter()
            .any(|stride| *stride < width as usize * 2 || stride % 4 != 0)
            || sizes.iter().any(|size| {
                *size > u64::from(wgpu::Limits::default().max_storage_buffer_binding_size)
            })
        {
            return Err("unsupported conversion buffer layout".into());
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wallpaper color conversion"),
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let failed = Arc::new(AtomicBool::new(false));
        let lost = failed.clone();
        device.set_device_lost_callback(move |_, _| {
            lost.store(true, Ordering::Relaxed);
        });
        let errors = failed.clone();
        device.on_uncaptured_error(Arc::new(move |_| {
            errors.store(true, Ordering::Relaxed);
        }));
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("P010 to RGBA"),
            source: wgpu::ShaderSource::Wgsl(include_str!("p010.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("P010 to RGBA"),
            layout: None,
            module: &shader,
            entry_point: Some("convert"),
            compilation_options: Default::default(),
            cache: None,
        });
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let y = buffer(
            "P010 luma",
            sizes[0],
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let uv = buffer(
            "P010 chroma",
            sizes[1],
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let output = buffer(
            "converted RGBA",
            sizes[2],
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = buffer(
            "converted RGBA readback",
            sizes[2],
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let parameters: Vec<u8> = [width, height, strides[0] as u32 / 4, strides[1] as u32 / 4]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("conversion layout"),
            contents: &parameters,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("conversion buffers"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: y.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uv.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        if let Some(error) = block_on(device.pop_error_scope()) {
            return Err(error.to_string());
        }
        eprintln!(
            "genkan wallpaper: GPU color conversion on {}",
            adapter.get_info().name
        );
        Ok(Self {
            device,
            queue,
            pipeline,
            bindings,
            y,
            uv,
            output,
            readback,
            width,
            height,
            strides,
            failed,
        })
    }

    fn convert(&self, frame: &video::VideoFrameRef<&gst::BufferRef>) -> Result<Bytes, String> {
        if self.failed.load(Ordering::Relaxed) {
            return Err("GPU conversion device failed".into());
        }
        for (index, buffer, rows) in [(0, &self.y, self.height), (1, &self.uv, self.height / 2)] {
            let plane = frame
                .plane_data(index as u32)
                .map_err(|error| error.to_string())?;
            let length = self.strides[index] * rows as usize;
            if frame.plane_stride()[index] as usize != self.strides[index] || plane.len() < length {
                return Err("conversion plane layout changed".into());
            }
            self.queue.write_buffer(buffer, 0, &plane[..length]);
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bindings, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(16), self.height.div_ceil(16), 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &self.readback, 0, self.readback.size());
        let submission = self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.readback
            .map_async(wgpu::MapMode::Read, .., move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(1)),
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let pixels = Bytes::copy_from_slice(&self.readback.get_mapped_range(..));
        self.readback.unmap();
        if self.failed.load(Ordering::Relaxed) {
            return Err("GPU conversion device failed".into());
        }
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_conversion_follows_midstream_caps_changes() {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        let source = gstreamer_app::AppSrc::builder()
            .format(gst::Format::Time)
            .build();
        let sink = gstreamer_app::AppSink::builder().sync(false).build();
        let rate = gst::ElementFactory::make("videorate").build().unwrap();
        let convert = gst::ElementFactory::make("videoconvert").build().unwrap();
        pipeline
            .add_many([source.upcast_ref(), &rate, &convert, sink.upcast_ref()])
            .unwrap();
        gst::Element::link_many([source.upcast_ref(), &rate, &convert, sink.upcast_ref()]).unwrap();
        configure(&source.static_pad("src").unwrap(), &sink);
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut sequence = 0;
        let cases = [
            (4, 4, video::VideoFormat::P01010le),
            (8, 4, video::VideoFormat::P01010le),
            (8, 6, video::VideoFormat::Nv12),
            (4, 2, video::VideoFormat::P01010le),
        ];
        for (width, height, format) in cases {
            let info = video::VideoInfo::builder(format, width, height)
                .fps((30, 1))
                .colorimetry(&"2:3:7:1".parse().unwrap())
                .chroma_site(video::VideoChromaSite::MPEG2)
                .build()
                .unwrap();
            for _ in 0..3 {
                let mut buffer = gst::Buffer::from_mut_slice(vec![0; info.size()]);
                buffer
                    .get_mut()
                    .unwrap()
                    .set_pts(gst::ClockTime::from_nseconds(sequence * 1_000_000_000 / 30));
                buffer
                    .get_mut()
                    .unwrap()
                    .set_duration(gst::ClockTime::from_nseconds(1_000_000_000 / 30));
                source
                    .push_sample(
                        &gst::Sample::builder()
                            .buffer(&buffer)
                            .caps(&info.to_caps().unwrap())
                            .build(),
                    )
                    .unwrap();
                sequence += 1;
            }
        }
        source.end_of_stream().unwrap();
        let mut observed = Vec::new();
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
            let info = video::VideoInfo::from_caps(sample.caps().unwrap()).unwrap();
            observed.push((info.width(), info.height(), info.format()));
        }
        let eos = sink.is_eos();
        let error = pipeline
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error]);
        pipeline.set_state(gst::State::Null).unwrap();
        assert!(error.is_none(), "{error:?}");
        assert!(eos);
        for (width, height, format) in cases {
            let expected = if format == video::VideoFormat::Nv12 {
                video::VideoFormat::Rgba
            } else {
                format
            };
            assert!(
                observed.contains(&(width, height, expected)),
                "{observed:?}"
            );
        }
    }

    // Run on Vulkan (including lavapipe) explicitly; a missing GPU must not
    // silently turn this differential test into a CPU-vs-CPU comparison.
    #[test]
    #[ignore = "requires a Vulkan adapter"]
    fn p010_gpu_matches_videoconvert() {
        gst::init().unwrap();
        for (width, height) in [(2, 2), (6, 10), (34, 18), (128, 66), (1920, 1080)] {
            for padding in [0, 16] {
                let strides = [width as i32 * 2 + padding, width as i32 * 2 + padding * 2];
                let offsets = [
                    padding as usize,
                    padding as usize + strides[0] as usize * height as usize,
                ];
                let size = offsets[1] + strides[1] as usize * (height / 2) as usize;
                let info = video::VideoInfo::builder(video::VideoFormat::P01010le, width, height)
                    .fps((30, 1))
                    .colorimetry(&"2:3:7:1".parse().unwrap())
                    .chroma_site(video::VideoChromaSite::MPEG2)
                    .stride(&strides)
                    .offset(&offsets)
                    .size(size)
                    .build()
                    .unwrap();
                assert!(supports(&info));
                let gpu = Gpu::new(&info, true).unwrap();
                for seed in [0u32, 1, 29, 0xdeadbeef] {
                    let mut state = seed;
                    let mut data = vec![0; info.size()];
                    for bytes in data.chunks_exact_mut(2) {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        // Include low-bit garbage: upstream expands the stored word
                        // without masking it. Seed zero checks a constant color.
                        let word = if seed == 29 {
                            state as u16
                        } else {
                            ((state & 1023) as u16) << 6
                        };
                        bytes.copy_from_slice(&word.to_le_bytes());
                    }
                    let buffer = gst::Buffer::from_mut_slice(data);
                    let frame =
                        video::VideoFrameRef::from_buffer_ref_readable(&buffer, &info).unwrap();
                    let expected = Converter {
                        gpu: None,
                        cpu: None,
                        disabled: true,
                        checked: false,
                    }
                    .convert(&frame)
                    .unwrap();
                    let actual = gpu.convert(&frame).unwrap();
                    assert_eq!(actual.len(), expected.len());
                    let difference = actual.iter().zip(expected.iter()).position(|(a, b)| a != b);
                    if let Some(index) = difference {
                        panic!("{width}x{height}, seed {seed}, byte {index}: GPU {} != CPU {} ({} differing bytes)", actual[index], expected[index], actual.iter().zip(expected.iter()).filter(|(a, b)| a != b).count());
                    }
                }
            }
        }
    }
}
