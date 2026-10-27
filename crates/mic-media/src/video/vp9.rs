//! VP9 both ways with libvpx (D10, §5.4), through `libvpx-native-sys`, the binding upstream's
//! decoder uses: real-time encoding, tuned for screen content when it's a screen, and decoding.
//!
//! libvpx is C, and the decoder parses what other participants send: a memory-safety risk the
//! design accepts for v1 (§5.4). Every packet it's given has been decrypted and confirmed as the
//! sender's first (D17, D18), so a stranger can't reach it, only a member of the room.

use std::ffi::c_int;
use std::ptr;

use vpx_sys as vpx;

use super::{Codec, CodecError, Decoder, Encoder, Packet, Picture};

fn error(why: impl Into<String>) -> CodecError {
    CodecError {
        codec: Codec::Vp9,
        why: why.into(),
    }
}

fn check(result: vpx::vpx_codec_err_t, what: &str) -> Result<(), CodecError> {
    if result == vpx::vpx_codec_err_t::VPX_CODEC_OK {
        Ok(())
    } else {
        Err(error(format!("{what}: {result:?}")))
    }
}

/// What a picture is encoded for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    /// A camera: natural motion.
    Camera,
    /// A screen: text, sharp edges, little motion (libvpx's screen-content tuning).
    Screen,
}

/// How a VP9 encoder runs.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target bitrate, kbit/s.
    pub kbps: u32,
    pub content: Content,
}

/// A real-time VP9 encoder: no lookahead, constant bitrate, keyframes only when asked.
pub struct Vp9Encoder {
    context: Box<vpx::vpx_codec_ctx_t>,
    settings: Settings,
    pts: i64,
}

// SAFETY: the context is only touched through `&mut self`, from one thread at a time, which
// libvpx allows for an encoder.
unsafe impl Send for Vp9Encoder {}

impl Vp9Encoder {
    pub fn new(settings: Settings) -> Result<Self, CodecError> {
        let mut config = vpx::vpx_codec_enc_cfg::default();
        // SAFETY: the interface is a static libvpx owns; the config is ours to fill.
        check(
            unsafe { vpx::vpx_codec_enc_config_default(vpx::vpx_codec_vp9_cx(), &mut config, 0) },
            "the encoder's defaults",
        )?;
        config.g_w = settings.width;
        config.g_h = settings.height;
        config.g_timebase = vpx::vpx_rational { num: 1, den: 1000 };
        config.g_lag_in_frames = 0;
        config.g_threads = std::thread::available_parallelism()
            .map_or(1, |cores| cores.get().min(4) as u32);
        config.rc_end_usage = vpx::vpx_rc_mode::VPX_CBR;
        config.rc_target_bitrate = settings.kbps;
        config.rc_min_quantizer = 4;
        config.rc_max_quantizer = 56;
        config.rc_buf_sz = 1000;
        config.rc_buf_initial_sz = 500;
        config.rc_buf_optimal_sz = 600;
        // Keyframes when a group starts, which the sender decides.
        config.kf_mode = vpx::vpx_kf_mode::VPX_KF_DISABLED;
        let mut context = Box::new(vpx::vpx_codec_ctx_t::default());
        // SAFETY: the context is fresh and boxed, so it doesn't move while libvpx holds it.
        check(
            unsafe {
                vpx::vpx_codec_enc_init_ver(
                    &mut *context,
                    vpx::vpx_codec_vp9_cx(),
                    &config,
                    0,
                    vpx::VPX_ENCODER_ABI_VERSION as c_int,
                )
            },
            "opening the encoder",
        )?;
        let mut encoder = Self {
            context,
            settings,
            pts: 0,
        };
        // Real time, as fast as libvpx's real-time mode goes short of its fastest.
        encoder.control(vpx::vp8e_enc_control_id::VP8E_SET_CPUUSED, 8)?;
        encoder.control(vpx::vp8e_enc_control_id::VP9E_SET_ROW_MT, 1)?;
        if settings.content == Content::Screen {
            encoder.control(
                vpx::vp8e_enc_control_id::VP9E_SET_TUNE_CONTENT,
                vpx::vp9e_tune_content::VP9E_CONTENT_SCREEN as c_int,
            )?;
        }
        Ok(encoder)
    }

    fn control(&mut self, id: vpx::vp8e_enc_control_id, value: c_int) -> Result<(), CodecError> {
        // SAFETY: an open context, and an integer control, as these ids take.
        check(
            unsafe { vpx::vpx_codec_control_(&mut *self.context, id as c_int, value) },
            "an encoder control",
        )
    }
}

impl Encoder for Vp9Encoder {
    fn codec(&self) -> Codec {
        Codec::Vp9
    }

    fn encode(&mut self, picture: &Picture, keyframe: bool) -> Result<Vec<Packet>, CodecError> {
        if (picture.width, picture.height) != (self.settings.width, self.settings.height) {
            return Err(error(format!(
                "a {}x{} picture for a {}x{} encoder",
                picture.width, picture.height, self.settings.width, self.settings.height
            )));
        }
        let mut image = vpx::vpx_image_t::default();
        // SAFETY: the image wraps the picture's packed I420 planes, which outlive the encode
        // call below; libvpx reads them and copies what it keeps.
        let wrapped = unsafe {
            vpx::vpx_img_wrap(
                &mut image,
                vpx::vpx_img_fmt::VPX_IMG_FMT_I420,
                picture.width,
                picture.height,
                1,
                picture.data.as_ptr().cast_mut(),
            )
        };
        if wrapped.is_null() {
            return Err(error("wrapping the picture"));
        }
        let duration = 1000 / u64::from(self.settings.fps.max(1));
        let flags = if keyframe {
            vpx::VPX_EFLAG_FORCE_KF as vpx::vpx_enc_frame_flags_t
        } else {
            0
        };
        // SAFETY: an open context and a wrapped image.
        check(
            unsafe {
                vpx::vpx_codec_encode(
                    &mut *self.context,
                    &image,
                    self.pts,
                    duration as _,
                    flags,
                    vpx::VPX_DL_REALTIME as _,
                )
            },
            "encoding",
        )?;
        self.pts += duration as i64;
        let mut packets = Vec::new();
        let mut iterator: vpx::vpx_codec_iter_t = ptr::null();
        loop {
            // SAFETY: an open context; the packet stays valid until the next call.
            let packet = unsafe { vpx::vpx_codec_get_cx_data(&mut *self.context, &mut iterator) };
            if packet.is_null() {
                break;
            }
            // SAFETY: non-null, and a frame packet's `frame` is the union's member to read.
            let packet = unsafe { &*packet };
            if packet.kind != vpx::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                continue;
            }
            let frame = unsafe { packet.data.frame };
            // SAFETY: libvpx hands `sz` bytes at `buf`, valid until the next call.
            let data = unsafe { std::slice::from_raw_parts(frame.buf.cast::<u8>(), frame.sz) };
            packets.push(Packet {
                data: data.to_vec(),
                keyframe: frame.flags & vpx::VPX_FRAME_IS_KEY != 0,
            });
        }
        Ok(packets)
    }
}

impl Drop for Vp9Encoder {
    fn drop(&mut self) {
        // SAFETY: an open context, closed once.
        unsafe { vpx::vpx_codec_destroy(&mut *self.context) };
    }
}

/// A VP9 decoder.
pub struct Vp9Decoder {
    context: Box<vpx::vpx_codec_ctx_t>,
}

// SAFETY: as for the encoder.
unsafe impl Send for Vp9Decoder {}

impl Vp9Decoder {
    pub fn new() -> Result<Self, CodecError> {
        let config = vpx::vpx_codec_dec_cfg {
            threads: 2,
            w: 0,
            h: 0,
        };
        let mut context = Box::new(vpx::vpx_codec_ctx_t::default());
        // SAFETY: as for the encoder.
        check(
            unsafe {
                vpx::vpx_codec_dec_init_ver(
                    &mut *context,
                    vpx::vpx_codec_vp9_dx(),
                    &config,
                    0,
                    vpx::VPX_DECODER_ABI_VERSION as c_int,
                )
            },
            "opening the decoder",
        )?;
        Ok(Self { context })
    }
}

impl Decoder for Vp9Decoder {
    fn codec(&self) -> Codec {
        Codec::Vp9
    }

    fn decode(&mut self, packet: &[u8]) -> Result<Option<Picture>, CodecError> {
        let length = u32::try_from(packet.len()).map_err(|_| error("a packet over 4 GiB"))?;
        // SAFETY: an open context, and `length` bytes at `packet`, which libvpx only reads.
        check(
            unsafe {
                vpx::vpx_codec_decode(
                    &mut *self.context,
                    packet.as_ptr(),
                    length,
                    ptr::null_mut(),
                    0,
                )
            },
            "decoding",
        )?;
        let mut iterator: vpx::vpx_codec_iter_t = ptr::null();
        let mut last = None;
        loop {
            // SAFETY: an open context; the image stays valid until the next decode.
            let image = unsafe { vpx::vpx_codec_get_frame(&mut *self.context, &mut iterator) };
            if image.is_null() {
                break;
            }
            // SAFETY: non-null.
            last = Some(unpack(unsafe { &*image })?);
        }
        Ok(last)
    }
}

/// A decoded image's planes, copied out tightly packed.
fn unpack(image: &vpx::vpx_image_t) -> Result<Picture, CodecError> {
    if image.fmt != vpx::vpx_img_fmt::VPX_IMG_FMT_I420 {
        return Err(error(format!("a picture in {:?}, not I420", image.fmt)));
    }
    let (width, height) = (image.d_w, image.d_h);
    let mut data = Vec::with_capacity((width * height * 3 / 2) as usize);
    for (plane, (w, h)) in [
        (0, (width, height)),
        (1, (width.div_ceil(2), height.div_ceil(2))),
        (2, (width.div_ceil(2), height.div_ceil(2))),
    ] {
        let stride = image.stride[plane] as usize;
        for row in 0..h as usize {
            // SAFETY: libvpx's plane holds `h` rows of `stride` bytes, `w` of them the picture.
            let line = unsafe {
                std::slice::from_raw_parts(image.planes[plane].add(row * stride), w as usize)
            };
            data.extend_from_slice(line);
        }
    }
    Ok(Picture {
        width,
        height,
        data,
    })
}

impl Drop for Vp9Decoder {
    fn drop(&mut self) {
        // SAFETY: an open context, closed once.
        unsafe { vpx::vpx_codec_destroy(&mut *self.context) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Pattern, read_pattern};

    fn round_trip(width: u32, height: u32, kbps: u32, content: Content) {
        let mut encoder = Vp9Encoder::new(Settings {
            width,
            height,
            fps: 30,
            kbps,
            content,
        })
        .unwrap();
        let mut decoder = Vp9Decoder::new().unwrap();
        let mut read = Vec::new();
        for frame in 0..60u16 {
            let picture = Pattern { sender: 5, frame }.draw(width, height);
            for packet in encoder.encode(&picture, frame % 30 == 0).unwrap() {
                assert_eq!(packet.keyframe, frame % 30 == 0, "frame {frame}");
                if let Some(decoded) = decoder.decode(&packet.data).unwrap() {
                    assert_eq!((decoded.width, decoded.height), (width, height));
                    read.extend(read_pattern(&decoded));
                }
            }
        }
        // Every frame decodes, and reads back as drawn, but perhaps the first, while the rate
        // control finds its feet.
        assert!(read.len() >= 59, "{} of 60 read back", read.len());
        assert!(read.iter().all(|pattern| pattern.sender == 5));
        assert_eq!(read.last().unwrap().frame, 59);
    }

    #[test]
    fn a_camera_pattern_survives_encoding_at_720p() {
        round_trip(1280, 720, 1500, Content::Camera);
    }

    #[test]
    fn and_at_180p_on_a_thin_bitrate() {
        round_trip(320, 180, 150, Content::Camera);
    }

    #[test]
    fn a_screen_encodes_too() {
        round_trip(1280, 720, 1000, Content::Screen);
    }

    #[test]
    fn what_isnt_vp9_is_an_error() {
        let mut decoder = Vp9Decoder::new().unwrap();
        assert!(decoder.decode(b"not a vp9 frame").is_err());
    }
}
