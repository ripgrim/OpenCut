#![cfg(feature = "wgpu-tests")]

use std::sync::Arc;

use decode::{Decoder, FakeDecoder, FakeDecoderConfig, PixelFormat, Source, SourceStreamId};
use render::pixels::readback_rgba8;
use render::{
    Affine, Blend, Crop, Node, NodeId, Opacity, Output, OutputFormat, RenderPlan, Renderer,
    SourceRef,
};
use time::{FrameRate, RationalTime};

/// See `plan.rs::headless_gpu` for the adapter selection rationale.
fn headless_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        let mut adapter = None;
        for force_fallback_adapter in [true, false] {
            adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter,
                    compatible_surface: None,
                })
                .await
                .ok();
            if adapter.is_some() {
                break;
            }
        }
        let adapter = adapter?;
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("compose_test_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .ok()
    })
}

fn output_8x8() -> Output {
    Output {
        width: 8,
        height: 8,
        format: OutputFormat::Rgba8Premultiplied,
    }
}

fn fill_affine(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> Affine {
    Affine {
        m: [
            out_w as f32 / src_w as f32,
            0.0,
            0.0,
            out_h as f32 / src_h as f32,
            0.0,
            0.0,
        ],
    }
}

fn decoder_with_color(color: [u8; 4], size: u32) -> FakeDecoder {
    FakeDecoder::new(FakeDecoderConfig {
        frame_rate: FrameRate::new(30, 1).unwrap(),
        width: size,
        height: size,
        duration: RationalTime::new(10, 1).unwrap(),
        format: PixelFormat::Rgba8,
        color,
    })
}

fn center_pixel(pixels: &[u8], width: u32, height: u32) -> [u8; 4] {
    let x = width / 2;
    let y = height / 2;
    let idx = ((y * width + x) * 4) as usize;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

#[test]
#[ignore = "requires GPU; run with --features wgpu-tests -- --ignored"]
fn single_opaque_node_fills_output_with_source_color() {
    let Some((device, queue)) = headless_gpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let mut decoder = decoder_with_color([200, 40, 60, 255], 8);
    let mut renderer = Renderer::new(&mut decoder, &device, &queue);
    let source = renderer
        .register_source(&Source::Bytes(Arc::from([0u8; 0])))
        .unwrap();
    let stream = SourceStreamId::new(1);

    let plan = RenderPlan {
        output: output_8x8(),
        nodes: vec![Node {
            id: NodeId::new(1),
            source: SourceRef {
                source,
                stream,
                time: RationalTime::new(0, 1).unwrap(),
            },
            crop: Crop {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
            transform: fill_affine(8, 8, 8, 8),
            opacity: Opacity::new(1.0).unwrap(),
            blend: Blend::SourceOver,
        }],
    };

    let out = renderer.render(&plan).unwrap();
    let pixels = readback_rgba8(&out.texture, &device, &queue, 8, 8).unwrap();
    let px = center_pixel(&pixels, 8, 8);
    assert_eq!(px, [200, 40, 60, 255]);
}

#[test]
#[ignore = "requires GPU; run with --features wgpu-tests -- --ignored"]
fn two_stacked_nodes_blend_with_half_opacity() {
    let Some((device, queue)) = headless_gpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let bottom = decoder_with_color([100, 0, 0, 255], 8);
    let top = decoder_with_color([0, 0, 200, 255], 8);

    /// Routes the first `open` to `bottom` and every later one to `top`. Each inner
    /// FakeDecoder numbers its sources from 1, so `top` ids are offset to stay unique.
    struct TwoDecoder {
        bottom: FakeDecoder,
        top: FakeDecoder,
        opened: usize,
    }

    const TOP_ID_OFFSET: u64 = 1_000;

    impl TwoDecoder {
        fn route(&self, source: decode::SourceId) -> (bool, decode::SourceId) {
            if source.raw() >= TOP_ID_OFFSET {
                (true, decode::SourceId::new(source.raw() - TOP_ID_OFFSET))
            } else {
                (false, source)
            }
        }
    }

    impl Decoder for TwoDecoder {
        fn open(&mut self, source: &Source) -> Result<decode::SourceId, decode::DecodeError> {
            self.opened += 1;
            if self.opened == 1 {
                self.bottom.open(source)
            } else {
                let inner = self.top.open(source)?;
                Ok(decode::SourceId::new(inner.raw() + TOP_ID_OFFSET))
            }
        }

        fn streams(
            &self,
            source: decode::SourceId,
        ) -> Result<Vec<decode::SourceStream>, decode::DecodeError> {
            match self.route(source) {
                (true, inner) => self.top.streams(inner),
                (false, inner) => self.bottom.streams(inner),
            }
        }

        fn read_frame(
            &mut self,
            source: decode::SourceId,
            stream: decode::SourceStreamId,
            time: time::RationalTime,
        ) -> Result<decode::Frame, decode::DecodeError> {
            match self.route(source) {
                (true, inner) => self.top.read_frame(inner, stream, time),
                (false, inner) => self.bottom.read_frame(inner, stream, time),
            }
        }

        fn close(&mut self, source: decode::SourceId) -> Result<(), decode::DecodeError> {
            match self.route(source) {
                (true, inner) => self.top.close(inner),
                (false, inner) => self.bottom.close(inner),
            }
        }
    }

    let mut decoder = TwoDecoder {
        bottom,
        top,
        opened: 0,
    };
    let mut renderer = Renderer::new(&mut decoder, &device, &queue);
    let bottom_source = renderer
        .register_source(&Source::Bytes(Arc::from([1u8; 0])))
        .unwrap();
    let top_source = renderer
        .register_source(&Source::Bytes(Arc::from([2u8; 0])))
        .unwrap();
    let bottom_stream = SourceStreamId::new(1);
    let top_stream = SourceStreamId::new(1);

    let plan = RenderPlan {
        output: output_8x8(),
        nodes: vec![
            Node {
                id: NodeId::new(1),
                source: SourceRef {
                    source: bottom_source,
                    stream: bottom_stream,
                    time: RationalTime::new(0, 1).unwrap(),
                },
                crop: Crop {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 8,
                },
                transform: fill_affine(8, 8, 8, 8),
                opacity: Opacity::new(1.0).unwrap(),
                blend: Blend::SourceOver,
            },
            Node {
                id: NodeId::new(2),
                source: SourceRef {
                    source: top_source,
                    stream: top_stream,
                    time: RationalTime::new(0, 1).unwrap(),
                },
                crop: Crop {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 8,
                },
                transform: fill_affine(8, 8, 8, 8),
                opacity: Opacity::new(0.5).unwrap(),
                blend: Blend::SourceOver,
            },
        ],
    };

    let out = renderer.render(&plan).unwrap();
    let pixels = readback_rgba8(&out.texture, &device, &queue, 8, 8).unwrap();
    let px = center_pixel(&pixels, 8, 8);

    // premul source-over: top(0,0,200,255)*0.5 over bottom(100,0,0,255)
    assert_eq!(px[0], 50);
    assert_eq!(px[2], 100);
    assert_eq!(px[3], 255);
}

#[test]
#[ignore = "requires GPU; run with --features wgpu-tests -- --ignored"]
fn cropped_node_renders_only_cropped_region() {
    let Some((device, queue)) = headless_gpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    /// 8x8 frame: left half red, right half green.
    struct HalfDecoder {
        inner: FakeDecoder,
    }

    impl Decoder for HalfDecoder {
        fn open(&mut self, source: &Source) -> Result<decode::SourceId, decode::DecodeError> {
            self.inner.open(source)
        }

        fn streams(
            &self,
            source: decode::SourceId,
        ) -> Result<Vec<decode::SourceStream>, decode::DecodeError> {
            self.inner.streams(source)
        }

        fn read_frame(
            &mut self,
            source: decode::SourceId,
            stream: decode::SourceStreamId,
            time: time::RationalTime,
        ) -> Result<decode::Frame, decode::DecodeError> {
            let mut frame = self.inner.read_frame(source, stream, time)?;
            let width = frame.width as usize;
            for (i, px) in frame.pixels.chunks_exact_mut(4).enumerate() {
                let x = i % width;
                px.copy_from_slice(if x < width / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 180, 0, 255]
                });
            }
            Ok(frame)
        }

        fn close(&mut self, source: decode::SourceId) -> Result<(), decode::DecodeError> {
            self.inner.close(source)
        }
    }

    let mut decoder = HalfDecoder {
        inner: decoder_with_color([0, 0, 0, 255], 8),
    };
    let mut renderer = Renderer::new(&mut decoder, &device, &queue);
    let source = renderer
        .register_source(&Source::Bytes(Arc::from([0u8; 0])))
        .unwrap();
    let stream = SourceStreamId::new(1);

    // Crop the green right half and place it at the output origin at 1:1.
    let plan = RenderPlan {
        output: output_8x8(),
        nodes: vec![Node {
            id: NodeId::new(1),
            source: SourceRef {
                source,
                stream,
                time: RationalTime::new(0, 1).unwrap(),
            },
            crop: Crop {
                x: 4,
                y: 0,
                w: 4,
                h: 8,
            },
            transform: fill_affine(4, 8, 4, 8),
            opacity: Opacity::new(1.0).unwrap(),
            blend: Blend::SourceOver,
        }],
    };

    let out = renderer.render(&plan).unwrap();
    let pixels = readback_rgba8(&out.texture, &device, &queue, 8, 8).unwrap();
    let px = |x: usize, y: usize| -> [u8; 4] {
        pixels[(y * 8 + x) * 4..][..4].try_into().unwrap()
    };

    // Inside the placed crop: green proves we sampled from x >= 4, not from x = 0.
    assert_eq!(px(0, 0), [0, 180, 0, 255]);
    assert_eq!(px(3, 7), [0, 180, 0, 255]);
    // Outside the crop's extent: untouched.
    assert_eq!(px(4, 0), [0, 0, 0, 0]);
    assert_eq!(px(7, 7), [0, 0, 0, 0]);
}

#[test]
#[ignore = "requires GPU; run with --features wgpu-tests -- --ignored"]
fn affine_transform_maps_source_to_expected_output_rectangle() {
    let Some((device, queue)) = headless_gpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let mut decoder = decoder_with_color([10, 20, 30, 255], 8);
    let mut renderer = Renderer::new(&mut decoder, &device, &queue);
    let source = renderer
        .register_source(&Source::Bytes(Arc::from([0u8; 0])))
        .unwrap();
    let stream = SourceStreamId::new(1);

    let plan = RenderPlan {
        output: output_8x8(),
        nodes: vec![Node {
            id: NodeId::new(1),
            source: SourceRef {
                source,
                stream,
                time: RationalTime::new(0, 1).unwrap(),
            },
            crop: Crop {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
            transform: Affine {
                m: [0.5, 0.0, 0.0, 0.5, 2.0, 2.0],
            },
            opacity: Opacity::new(1.0).unwrap(),
            blend: Blend::SourceOver,
        }],
    };

    let out = renderer.render(&plan).unwrap();
    let pixels = readback_rgba8(&out.texture, &device, &queue, 8, 8).unwrap();

    let inside = center_pixel(&pixels, 8, 8);
    let outside_idx = (0 * 8 + 0) * 4;
    assert_eq!(inside, [10, 20, 30, 255]);
    assert_eq!(pixels[outside_idx..outside_idx + 4], [0, 0, 0, 0]);
}
