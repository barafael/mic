//! The video-tile spike (PLAN.md, commit 22; design §5.10): a Slint window with tiles of
//! `moq-video` frames at a frame rate, drawn on Slint's own wgpu device, and what each frame costs.
//!
//! - `--source cpu`: I420 frames uploaded from memory, as a software decoder (libvpx, for VP9)
//!   hands them over.
//! - `--source vaapi --h264 <file>`: an H.264 stream decoded on VA-API for every tile, so the frames
//!   are DMA-BUFs, imported zero-copy if the device can. Make one with
//!   `ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -t 2 -pix_fmt yuv420p -c:v libx264
//!   -profile:v main -bf 0 -g 30 -x264-params aud=1 -f h264 pattern-720p.h264`.
//!
//! It prints, once a second and at the end: frames drawn, the CPU each tick spends decoding and
//! converting, the time Slint spends building a frame, the time from a tick to the GPU being done
//! with it, the process's CPU time per frame, and the GPU's busy share against a baseline taken
//! before the tiles start.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moq_video::render::{self, Renderer, wgpu};
use moq_video::{Frame, I420, Size, Surface};
use slint::{ComponentHandle, Model, VecModel};

slint::slint! {
    export component Tiles inherits Window {
        in property <[image]> tiles;
        in property <int> columns: 3;
        in property <string> stats;
        title: "video-tiles";
        preferred-width: 1280px;
        preferred-height: 720px;
        background: black;
        for tile[i] in root.tiles: Image {
            x: root.width / root.columns * Math.mod(i, root.columns);
            y: root.height / Math.ceil(root.tiles.length / root.columns) * Math.floor(i / root.columns);
            width: root.width / root.columns;
            height: root.height / Math.ceil(root.tiles.length / root.columns);
            source: tile;
            image-fit: contain;
        }
        Text {
            x: 8px;
            y: 8px;
            text: root.stats;
            color: white;
            font-size: 14px;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Cpu,
    Vaapi,
}

struct Args {
    source: Source,
    size: Size,
    tiles: usize,
    fps: u32,
    seconds: u64,
    h264: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        source: Source::Cpu,
        size: Size::new(1280, 720),
        tiles: 9,
        fps: 30,
        seconds: 20,
        h264: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--source" => {
                args.source = match value()?.as_str() {
                    "cpu" => Source::Cpu,
                    "vaapi" => Source::Vaapi,
                    other => return Err(format!("no source {other}: cpu or vaapi")),
                }
            }
            "--size" => {
                args.size = match value()?.as_str() {
                    "720p" => Size::new(1280, 720),
                    "360p" => Size::new(640, 360),
                    "180p" => Size::new(320, 180),
                    other => return Err(format!("no size {other}: 720p, 360p or 180p")),
                }
            }
            "--h264" => args.h264 = Some(value()?),
            "--tiles" => args.tiles = value()?.parse().map_err(|e| format!("--tiles: {e}"))?,
            "--fps" => args.fps = value()?.parse().map_err(|e| format!("--fps: {e}"))?,
            "--seconds" => {
                args.seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

/// Frames in the pattern's loop: a second at 30 fps.
const LOOP: usize = 30;

/// The pattern: a gradient with a bar sweeping across it, once per loop.
fn pattern(size: Size) -> Vec<I420> {
    let (w, h) = (size.width as usize, size.height as usize);
    (0..LOOP)
        .map(|f| {
            let mut data = vec![0u8; I420::len(size).unwrap()];
            let (y, uv) = data.split_at_mut(w * h);
            let bar = f * w / LOOP;
            for row in 0..h {
                for col in 0..w {
                    y[row * w + col] = if col.abs_diff(bar) < w / 40 {
                        235
                    } else {
                        (16 + (row + col) * 200 / (w + h)) as u8
                    };
                }
            }
            let (u, v) = uv.split_at_mut(w / 2 * h / 2);
            u.fill(90);
            v.fill(170);
            I420::new(size, data).unwrap()
        })
        .collect()
}

fn timestamp(i: usize, fps: u32) -> moq_net::Timestamp {
    moq_net::Timestamp::from_micros(i as u64 * 1_000_000 / u64::from(fps)).unwrap()
}

/// What a tile decodes from.
enum Feed {
    /// Frames as a software decoder hands them over.
    Cpu(Rc<Vec<Frame>>),
    /// A decoder of its own, and the shared bitstream: access units, and whether each is a
    /// keyframe.
    Vaapi(moq_video::decode::Decoder, Rc<Vec<(bytes::Bytes, bool)>>),
}

struct Tile {
    feed: Feed,
    renderer: Renderer,
    next: usize,
}

impl Tile {
    /// Draws its next frame, and hands back the texture holding it: none while a decoder holds
    /// its pictures back.
    fn tick(&mut self, fps: u32) -> Option<wgpu::Texture> {
        let at = self.next;
        self.next += 1;
        match &mut self.feed {
            Feed::Cpu(frames) => Some(self.renderer.render(&frames[at % frames.len()]).unwrap()),
            Feed::Vaapi(decoder, units) => {
                let (unit, keyframe) = &units[at % units.len()];
                let frames = decoder.decode(unit, timestamp(at, fps), *keyframe).unwrap();
                frames
                    .last()
                    .map(|frame| self.renderer.render(frame).unwrap())
            }
        }
    }
}

/// An Annex-B H.264 stream with access unit delimiters, split at them, and its catalog rendition
/// from its SPS.
///
/// Not encoded here: `moq-video`'s VA-API encoder emits no parameter sets on this machine (Mesa
/// 26.2.5, radeonsi), though ffmpeg's `h264_vaapi` does on the same driver.
fn stream(path: &str) -> (hang::catalog::VideoConfig, Vec<(bytes::Bytes, bool)>) {
    let stream = bytes::Bytes::from(std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}")));
    // Where each NAL unit's header is, and its type.
    let nals: Vec<(usize, u8)> = stream
        .windows(4)
        .enumerate()
        .filter(|(_, w)| w[..3] == [0, 0, 1])
        .map(|(at, w)| (at + 3, w[3] & 0x1f))
        .collect();
    let delimiters: Vec<usize> = nals
        .iter()
        .filter(|(_, kind)| *kind == 9)
        .map(|(at, _)| at - 3)
        .chain([stream.len()])
        .collect();
    assert!(
        delimiters.len() > 1,
        "{path} has no access unit delimiters (x264: aud=1)"
    );
    let units = delimiters
        .windows(2)
        .map(|w| {
            let unit = stream.slice(w[0]..w[1]);
            let keyframe = nals
                .iter()
                .any(|&(at, kind)| kind == 5 && (w[0]..w[1]).contains(&at));
            (unit, keyframe)
        })
        .collect();
    let &(sps, _) = nals.iter().find(|(_, kind)| *kind == 7).expect("an SPS");
    let catalog = hang::catalog::VideoConfig::new(hang::catalog::H264 {
        inline: true,
        profile: stream[sps + 1],
        constraints: stream[sps + 2],
        level: stream[sps + 3],
    });
    (catalog, units)
}

/// The device: Vulkan, and DMA-BUF import where the adapter has it.
fn device() -> (wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("a Vulkan adapter");
    let info = adapter.get_info();
    let dma_buf = adapter
        .features()
        .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF);
    println!(
        "adapter: {} ({:?}, {}), DMA-BUF import: {}",
        info.name,
        info.device_type,
        info.driver_info,
        if dma_buf { "offered" } else { "not offered" }
    );
    let features = if dma_buf {
        wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("video-tiles"),
        required_features: features,
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .expect("a device");
    (instance, adapter, device, queue)
}

/// The GPU's busy share, in percent, sampled from amdgpu's sysfs file every 50 ms.
struct Busy {
    samples: Arc<Mutex<Vec<u8>>>,
}

impl Busy {
    fn start() -> Option<Self> {
        let path = std::fs::read_dir("/sys/class/drm")
            .ok()?
            .flatten()
            .map(|card| card.path().join("device/gpu_busy_percent"))
            .find(|path| path.exists())?;
        let samples = Arc::new(Mutex::new(Vec::new()));
        let into = samples.clone();
        std::thread::spawn(move || {
            loop {
                if let Some(busy) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|busy| busy.trim().parse().ok())
                {
                    into.lock().unwrap().push(busy);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Some(Self { samples })
    }

    /// The mean since the last call.
    fn take(&self) -> f64 {
        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        samples.iter().map(|&s| f64::from(s)).sum::<f64>() / samples.len().max(1) as f64
    }
}

/// The process's CPU time, user and system.
fn cpu() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage fills the struct it's given.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let time = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// Durations, for a mean and a 95th percentile.
#[derive(Default)]
struct Times(Vec<Duration>);

impl Times {
    fn summary(&self) -> String {
        if self.0.is_empty() {
            return "-".to_owned();
        }
        let mut sorted = self.0.clone();
        sorted.sort();
        let mean = sorted.iter().sum::<Duration>() / sorted.len() as u32;
        let p95 = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)];
        format!("{:.2}/{:.2} ms", ms(mean), ms(p95))
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// What a stretch of the run measured.
#[derive(Default)]
struct Stats {
    ticks: usize,
    drawn: usize,
    tick: Times,
    build: Times,
    gpu: Times,
    busy: Vec<f64>,
}

impl Stats {
    fn line(&self, elapsed: Duration, cpu: Duration, baseline: f64) -> String {
        let secs = elapsed.as_secs_f64();
        let busy = self.busy.iter().sum::<f64>() / self.busy.len().max(1) as f64;
        format!(
            "{:.1} fps drawn, {:.1} ticks/s; tick (decode + convert) {}; Slint build {}; \
             tick to GPU done {}; process CPU {:.2} ms/frame ({:.0}% of a core); \
             GPU busy {busy:.0}% (baseline {baseline:.0}%)",
            self.drawn as f64 / secs,
            self.ticks as f64 / secs,
            self.tick.summary(),
            self.build.summary(),
            self.gpu.summary(),
            ms(cpu) / self.ticks.max(1) as f64,
            cpu.as_secs_f64() / secs * 100.0,
        )
    }
}

/// The last second's measurements, and the whole run's.
#[derive(Default)]
struct Run {
    second: Stats,
    total: Stats,
}

impl Run {
    fn both(&mut self, record: impl Fn(&mut Stats)) {
        record(&mut self.second);
        record(&mut self.total);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = args()?;
    let (instance, adapter, device, queue) = device();
    slint::BackendSelector::new()
        .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::Manual {
            instance,
            adapter,
            device: device.clone(),
            queue: queue.clone(),
        })
        .select()?;
    let window = Tiles::new()?;
    window.set_columns((args.tiles as f64).sqrt().ceil() as i32);

    let feed: Box<dyn Fn() -> Feed> = match args.source {
        Source::Cpu => {
            let frames: Rc<Vec<Frame>> = Rc::new(
                pattern(args.size)
                    .into_iter()
                    .enumerate()
                    .map(|(i, i420)| Frame::new(Surface::I420(i420), timestamp(i, args.fps)))
                    .collect(),
            );
            Box::new(move || Feed::Cpu(frames.clone()))
        }
        Source::Vaapi => {
            let path = args
                .h264
                .as_deref()
                .ok_or("--source vaapi needs --h264 <file>")?;
            let (catalog, units) = stream(path);
            println!(
                "read {} access units, {} keyframes, {} bytes",
                units.len(),
                units.iter().filter(|(_, keyframe)| *keyframe).count(),
                units.iter().map(|(unit, _)| unit.len()).sum::<usize>()
            );
            let units = Rc::new(units);
            Box::new(move || {
                let mut config = moq_video::decode::Config::new();
                config.kind = moq_video::decode::Kind::Named("vaapi".to_owned());
                let decoder = moq_video::decode::Decoder::new(&catalog, &config).unwrap();
                Feed::Vaapi(decoder, units.clone())
            })
        }
    };
    let tiles: Vec<Tile> = (0..args.tiles)
        .map(|i| Tile {
            feed: feed(),
            renderer: Renderer::new(&device, &queue, render::Config::new()).unwrap(),
            // Each tile a third of a second apart, so they don't move as one.
            next: i * LOOP / args.tiles,
        })
        .collect();
    let tiles = Rc::new(RefCell::new(tiles));
    let images = Rc::new(VecModel::from(vec![slint::Image::default(); args.tiles]));
    window.set_tiles(images.clone().into());

    let run = Arc::new(Mutex::new(Run::default()));
    let busy = Busy::start();
    if busy.is_none() {
        println!("no gpu_busy_percent in /sys/class/drm: GPU busy isn't measured");
    }
    // When the tiles last started drawing: what the GPU's done is timed from.
    let tick_began = Rc::new(RefCell::new(None::<Instant>));

    // Slint's own work per frame, and when the GPU is done with it.
    {
        let run = run.clone();
        let tick_began = tick_began.clone();
        let queue = queue.clone();
        let (waits, waited) = std::sync::mpsc::channel::<(wgpu::SubmissionIndex, Instant)>();
        {
            let device = device.clone();
            let run = run.clone();
            std::thread::spawn(move || {
                for (submission, tick) in waited {
                    let _ = device.poll(wgpu::PollType::Wait {
                        submission_index: Some(submission),
                        timeout: None,
                    });
                    let done = tick.elapsed();
                    run.lock().unwrap().both(|stats| stats.gpu.0.push(done));
                }
            });
        }
        let mut building = None;
        window
            .window()
            .set_rendering_notifier(move |state, _| match state {
                slint::RenderingState::BeforeRendering => building = Some(Instant::now()),
                slint::RenderingState::AfterRendering => {
                    let Some(tick) = *tick_began.borrow() else {
                        return;
                    };
                    let build = building.take().map(|building| building.elapsed());
                    run.lock().unwrap().both(|stats| {
                        stats.drawn += 1;
                        stats.build.0.extend(build);
                    });
                    // An empty submission after Slint's: the queue runs in order, so it's done when
                    // Slint's frame is. Polled on a thread of its own, since nothing else polls.
                    let submission = queue.submit([]);
                    let _ = waits.send((submission, tick));
                }
                _ => {}
            })?;
    }

    let timer = slint::Timer::default();
    {
        let window = window.as_weak();
        let fps = args.fps;
        let seconds = args.seconds;
        let up = Instant::now();
        // Set once the tiles start: when, the CPU used by then, and the GPU's baseline.
        let mut started = None::<(Instant, Duration, f64)>;
        let mut second = (Instant::now(), cpu());
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs_f64(1.0 / f64::from(fps)),
            move || {
                // Two seconds of the window standing still, for the baseline.
                if up.elapsed() < Duration::from_secs(2) {
                    return;
                }
                let (start, start_cpu, baseline) = *started.get_or_insert_with(|| {
                    let baseline = busy.as_ref().map_or(0.0, Busy::take);
                    second = (Instant::now(), cpu());
                    (Instant::now(), cpu(), baseline)
                });
                let tick = Instant::now();
                *tick_began.borrow_mut() = Some(tick);
                for (i, tile) in tiles.borrow_mut().iter_mut().enumerate() {
                    if let Some(texture) = tile.tick(fps) {
                        images.set_row_data(i, slint::Image::try_from(texture).unwrap());
                    }
                }
                let took = tick.elapsed();
                let mut run = run.lock().unwrap();
                run.both(|stats| {
                    stats.ticks += 1;
                    stats.tick.0.push(took);
                });
                if second.0.elapsed() < Duration::from_secs(1) {
                    return;
                }
                let busy = busy.as_ref().map_or(0.0, Busy::take);
                run.both(|stats| stats.busy.push(busy));
                let line = run
                    .second
                    .line(second.0.elapsed(), cpu() - second.1, baseline);
                println!("{line}");
                if let Some(window) = window.upgrade() {
                    window.set_stats(line.replace("; ", "\n").into());
                }
                run.second = Stats::default();
                second = (Instant::now(), cpu());
                if start.elapsed() >= Duration::from_secs(seconds) {
                    let total = run.total.line(start.elapsed(), cpu() - start_cpu, baseline);
                    println!("total: {total}");
                    let _ = slint::quit_event_loop();
                }
            },
        );
    }
    window.run()?;
    Ok(())
}
