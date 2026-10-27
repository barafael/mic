//! A tile drawn on a GPU, offscreen, and read back: the pattern's pixels, as the CPU path has
//! them. On whatever adapter there is (lavapipe where there's no GPU); skipped without one unless
//! `MIC_TEST_GPU=1` says there must be one.

use mic_media::video::{Pattern, Picture, read_pattern};
use mic_render::{Tile, rgba, wgpu};

/// A device on the first adapter there is of a primary backend (Vulkan, Metal, DX12), if any.
/// Not OpenGL: moq-video's renderer wants view formats, which wgpu's GL backend hasn't
/// (docs/observations.md).
fn device() -> Option<(wgpu::Device, wgpu::Queue, String)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: false,
        ..Default::default()
    }))
    .ok()?;
    let name = adapter.get_info().name;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some((device, queue, name))
}

/// `texture`'s RGBA pixels, copied back.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let (width, height) = (texture.width(), texture.height());
    let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |mapped| {
        mapped.expect("the buffer maps")
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the copy completes");
    let mapped = slice.get_mapped_range().expect("the buffer is mapped");
    (0..height as usize)
        .flat_map(|y| mapped[y * row as usize..][..width as usize * 4].to_vec())
        .collect()
}

#[test]
fn a_tile_on_the_gpu_shows_the_pattern_as_the_cpu_does() {
    let Some((device, queue, adapter)) = device() else {
        assert!(
            std::env::var("MIC_TEST_GPU").as_deref() != Ok("1"),
            "no adapter, and MIC_TEST_GPU=1 wants one"
        );
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let picture = Pattern {
        sender: 7,
        frame: 321,
    }
    .draw(640, 360);
    let mut tile = Tile::new(&device, &queue, true).unwrap();
    let texture = tile.draw(&picture).unwrap();
    assert_eq!((texture.width(), texture.height()), (640, 360));
    let gpu = read_back(&device, &queue, &texture);
    let cpu = rgba(&picture);
    // As the CPU has it, give or take rounding and the shader's chroma filtering at edges.
    let differences: Vec<u8> = gpu.iter().zip(&cpu).map(|(a, b)| a.abs_diff(*b)).collect();
    let mean = differences.iter().map(|&d| f64::from(d)).sum::<f64>() / differences.len() as f64;
    assert!(mean < 3.0, "{adapter}: {mean:.2} apart on average");
    // And it reads back: the luma of what the GPU drew holds the pattern.
    let luma: Vec<u8> = gpu
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| ((u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8) as u8)
        .collect();
    let mut drawn = Picture::grey(640, 360);
    drawn.y_mut().copy_from_slice(&luma);
    assert_eq!(
        read_pattern(&drawn).map(|p| (p.sender, p.frame)),
        Some((7, 321))
    );
}
