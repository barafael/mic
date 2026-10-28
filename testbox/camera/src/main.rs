//! The test box's camera: a PipeWire `Video/Source` with the Camera role, as PipeWire shows a
//! webcam, drawing mic-media's pattern for a sender, so a check knows whose camera arrived and
//! that it moves. YUY2, as webcams send, at the size and rate mic sends a camera at; it drives
//! itself, a picture a period, there being no other video to clock it. It runs until it's
//! killed: a check starts it in carol's pod, where her window finds it as a camera.

#[cfg(target_os = "linux")]
fn main() -> Result<(), String> {
    use clap::Parser as _;
    camera::run(&camera::Cli::parse()).map_err(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("box-camera: PipeWire is Linux's");
}

#[cfg(target_os = "linux")]
mod camera {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use mic_media::video::{Pattern, Picture};
    use pipewire as pw;
    use pw::properties::properties;
    use pw::spa;

    #[derive(clap::Parser)]
    pub struct Cli {
        /// Whose pattern it shows (mic-media's `Pattern`): apart from every synthetic camera's.
        #[arg(long)]
        pub sender: u8,
        /// Its PipeWire node's name: `pipewire:<name>` to mic.
        #[arg(long, default_value = "box-camera")]
        pub name: String,
        #[arg(long, default_value_t = 640)]
        pub width: u32,
        #[arg(long, default_value_t = 360)]
        pub height: u32,
        #[arg(long, default_value_t = 15)]
        pub fps: u32,
    }

    pub fn run(cli: &Cli) -> Result<(), pw::Error> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;
        let description = format!("The box's camera ({})", cli.sender);
        let stream = pw::stream::StreamRc::new(
            core,
            "box-camera",
            properties! {
                *pw::keys::MEDIA_CLASS => "Video/Source",
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Camera",
                *pw::keys::NODE_NAME => cli.name.as_str(),
                *pw::keys::NODE_DESCRIPTION => description.as_str(),
            },
        )?;
        let (width, height, sender) = (cli.width, cli.height, cli.sender);
        let frame = Rc::new(Cell::new(0u16));
        // Whether it's linked and streaming: before, there's nothing to trigger.
        let streaming = Rc::new(Cell::new(false));
        let _listener = stream
            .add_local_listener_with_user_data(frame)
            .state_changed({
                let streaming = streaming.clone();
                move |_, _, _, state| {
                    streaming.set(matches!(state, pw::stream::StreamState::Streaming));
                }
            })
            // The format taken, the buffers it needs: a picture's worth each, in shared memory.
            .param_changed(move |stream, _, id, param| {
                if param.is_none() || id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let buffers = buffers(width * 2, height);
                let mut params = [spa::pod::Pod::from_bytes(&buffers).expect("a buffers pod")];
                if let Err(error) = stream.update_params(&mut params) {
                    eprintln!("box-camera: the buffers: {error}");
                }
            })
            .process(move |stream, frame| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let picture = Pattern {
                    sender,
                    frame: frame.get(),
                }
                .draw(width, height);
                frame.set(frame.get().wrapping_add(1));
                let stride = width * 2;
                let data = &mut buffer.datas_mut()[0];
                let written = data.data().map_or(0, |bytes| yuy2(&picture, bytes));
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = stride as i32;
                *chunk.size_mut() = written as u32;
            })
            .register()?;
        let format = format(cli.width, cli.height, cli.fps);
        let mut params = [spa::pod::Pod::from_bytes(&format).expect("a format pod")];
        stream.connect(
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;
        // A picture a period: the driver says when.
        let period = Duration::from_secs(1) / cli.fps.max(1);
        let ticking = stream.clone();
        let timer = mainloop.loop_().add_timer(move |_| {
            if streaming.get()
                && let Err(error) = ticking.trigger_process()
            {
                eprintln!("box-camera: {error}");
            }
        });
        timer.update_timer(Some(period), Some(period)).into_result()?;
        mainloop.run();
        Ok(())
    }

    /// `picture`, I420, written into `bytes` as YUY2 (Y0 U Y1 V), as much as fits; how much.
    fn yuy2(picture: &Picture, bytes: &mut [u8]) -> usize {
        let (width, height) = (picture.width as usize, picture.height as usize);
        let (luma, chroma) = picture.data.split_at(width * height);
        let (us, vs) = chroma.split_at(width / 2 * height.div_ceil(2));
        let mut at = 0;
        for row in 0..height {
            for pair in 0..width / 2 {
                if at + 4 > bytes.len() {
                    return at;
                }
                let c = (row / 2) * (width / 2) + pair;
                bytes[at] = luma[row * width + pair * 2];
                bytes[at + 1] = us[c];
                bytes[at + 2] = luma[row * width + pair * 2 + 1];
                bytes[at + 3] = vs[c];
                at += 4;
            }
        }
        at
    }

    /// The buffers a picture of `stride` bytes a row and `height` rows takes, in shared memory.
    fn buffers(stride: u32, height: u32) -> Vec<u8> {
        let int = |value: u32| spa::pod::Value::Int(value as i32);
        let data_types = (1 << spa::buffer::DataType::MemPtr.as_raw())
            | (1 << spa::buffer::DataType::MemFd.as_raw());
        let object = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
            id: spa::param::ParamType::Buffers.as_raw(),
            properties: vec![
                spa::pod::Property::new(spa::sys::SPA_PARAM_BUFFERS_buffers, int(4)),
                spa::pod::Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, int(1)),
                spa::pod::Property::new(spa::sys::SPA_PARAM_BUFFERS_size, int(stride * height)),
                spa::pod::Property::new(spa::sys::SPA_PARAM_BUFFERS_stride, int(stride)),
                spa::pod::Property::new(
                    spa::sys::SPA_PARAM_BUFFERS_dataType,
                    spa::pod::Value::Int(data_types),
                ),
            ],
        };
        spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(object),
        )
        .expect("a static buffers pod serializes")
        .0
        .into_inner()
    }

    /// The one format it offers: YUY2, `width` by `height`, `fps` a second.
    fn format(width: u32, height: u32, fps: u32) -> Vec<u8> {
        let object = spa::pod::object!(
            spa::utils::SpaTypes::ObjectParamFormat,
            spa::param::ParamType::EnumFormat,
            spa::pod::property!(
                spa::param::format::FormatProperties::MediaType,
                Id,
                spa::param::format::MediaType::Video
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::MediaSubtype,
                Id,
                spa::param::format::MediaSubtype::Raw
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFormat,
                Id,
                spa::param::video::VideoFormat::YUY2
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoSize,
                Rectangle,
                spa::utils::Rectangle { width, height }
            ),
            spa::pod::property!(
                spa::param::format::FormatProperties::VideoFramerate,
                Fraction,
                spa::utils::Fraction { num: fps, denom: 1 }
            ),
        );
        spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(object),
        )
        .expect("a static format pod serializes")
        .0
        .into_inner()
    }
}
