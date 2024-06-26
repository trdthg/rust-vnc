use byteorder::{NativeEndian, ReadBytesExt, WriteBytesExt};
use clap::{arg, value_parser, Arg, ArgAction, Command};
use log::{debug, error, info, warn};
use sdl2::{
    pixels::{Color, PixelFormatEnum as SdlPixelFormat, PixelMasks},
    rect::Rect as SdlRect,
};
use std::io::{Cursor, ErrorKind as IoErrorKind, Read, Result as IoResult, Write};

use std::time::Duration;

const FORMAT_MAP: [(SdlPixelFormat, t_vnc::PixelFormat); 5] = [
    (
        SdlPixelFormat::RGB888,
        t_vnc::PixelFormat {
            bits_per_pixel: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        },
    ),
    (
        SdlPixelFormat::BGR888,
        t_vnc::PixelFormat {
            bits_per_pixel: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 0,
            green_shift: 8,
            blue_shift: 16,
        },
    ),
    // these break x11vnc
    // (SdlPixelFormat::RGB24, t_vnc::PixelFormat {
    //     bits_per_pixel: 24, depth: 24, big_endian: false, true_colour: true,
    //     red_max: 255,  green_max: 255, blue_max: 255,
    //     red_shift: 16, green_shift: 8, blue_shift: 0
    // }),
    // (SdlPixelFormat::BGR24, t_vnc::PixelFormat {
    //     bits_per_pixel: 24, depth: 24, big_endian: true, true_colour: true,
    //     red_max: 255,  green_max: 255, blue_max: 255,
    //     red_shift: 0, green_shift: 8, blue_shift: 16
    // }),
    (
        SdlPixelFormat::RGB565,
        t_vnc::PixelFormat {
            bits_per_pixel: 16,
            depth: 16,
            big_endian: false,
            true_colour: true,
            red_max: 32,
            green_max: 64,
            blue_max: 32,
            red_shift: 11,
            green_shift: 5,
            blue_shift: 0,
        },
    ),
    (
        SdlPixelFormat::BGR565,
        t_vnc::PixelFormat {
            bits_per_pixel: 16,
            depth: 16,
            big_endian: false,
            true_colour: true,
            red_max: 32,
            green_max: 64,
            blue_max: 32,
            red_shift: 0,
            green_shift: 5,
            blue_shift: 11,
        },
    ),
    (
        SdlPixelFormat::RGB332,
        t_vnc::PixelFormat {
            bits_per_pixel: 8,
            depth: 8,
            big_endian: false,
            true_colour: true,
            red_max: 8,
            green_max: 8,
            blue_max: 4,
            red_shift: 5,
            green_shift: 2,
            blue_shift: 0,
        },
    ),
];

fn pixel_format_vnc_to_sdl(vnc_format: t_vnc::PixelFormat) -> Option<SdlPixelFormat> {
    for format in &FORMAT_MAP {
        if format.1 == vnc_format {
            return Some(format.0);
        }
    }
    None
}

fn pixel_format_sdl_to_vnc(sdl_format: SdlPixelFormat) -> Option<t_vnc::PixelFormat> {
    for format in &FORMAT_MAP {
        if format.0 == sdl_format {
            return Some(format.1);
        }
    }
    None
}

fn mask_cursor(
    vnc_in_format: t_vnc::PixelFormat,
    in_pixels: Vec<u8>,
    mask_pixels: Vec<u8>,
) -> (SdlPixelFormat, Vec<u8>) {
    use sdl2::pixels::PixelFormatEnum::*;

    let in_format = pixel_format_vnc_to_sdl(vnc_in_format).unwrap();
    let out_format = match in_format {
        RGB332 => ARGB4444, /* meh, close enough */
        RGB444 => ARGB4444,
        RGB555 | RGB565 => ARGB1555,
        BGR555 | BGR565 => ABGR1555,
        RGB24 | RGB888 | RGBX8888 => ARGB8888,
        BGR24 | BGR888 | BGRX8888 => ABGR8888,
        _ => panic!("cannot add alpha to {:?}", in_format),
    };
    let out_pixels = Vec::new();

    let in_size = in_format.byte_size_per_pixel();
    let in_masks = in_format.into_masks().unwrap();
    let out_size = out_format.byte_size_per_pixel();
    let out_masks = out_format.into_masks().unwrap();

    let mut in_cursor = Cursor::new(in_pixels);
    let mut out_cursor = Cursor::new(out_pixels);
    let mut mask_cursor = Cursor::new(mask_pixels);

    fn read_color<R: Read>(reader: &mut R, size: usize, masks: &PixelMasks) -> IoResult<Color> {
        let packed = reader.read_uint::<NativeEndian>(size)?;
        Ok(Color::RGB(
            ((packed as u32 & masks.rmask) >> masks.rmask.trailing_zeros()) as u8,
            ((packed as u32 & masks.gmask) >> masks.gmask.trailing_zeros()) as u8,
            ((packed as u32 & masks.bmask) >> masks.bmask.trailing_zeros()) as u8,
        ))
    }

    fn write_color<W: Write>(
        writer: &mut W,
        size: usize,
        masks: &PixelMasks,
        color: Color,
    ) -> IoResult<()> {
        let packed = match color {
            Color { r, g, b, a } => {
                (((r as u32) << masks.rmask.trailing_zeros()) & masks.rmask)
                    | (((g as u32) << masks.gmask.trailing_zeros()) & masks.gmask)
                    | (((b as u32) << masks.bmask.trailing_zeros()) & masks.bmask)
                    | (((a as u32) << masks.amask.trailing_zeros()) & masks.amask)
            }
        };
        writer
            .write_uint::<NativeEndian>(packed as u64, size)
            .unwrap();
        Ok(())
    }

    loop {
        match read_color(&mut in_cursor, in_size, &in_masks) {
            Err(ref e) if e.kind() == IoErrorKind::UnexpectedEof => break,
            Err(_) => unreachable!(),
            Ok(in_color) => {
                let mask = mask_cursor.read_u8().unwrap();
                let out_color = match in_color {
                    Color { r, g, b, .. } => Color::RGBA(r, g, b, if mask != 0 { 255 } else { 0 }),
                };
                write_color(&mut out_cursor, out_size, &out_masks, out_color).unwrap();
            }
        }
    }

    (out_format, out_cursor.into_inner())
}

fn main() {
    env_logger::init();

    let matches = Command::new("rvncclient")
        .about("VNC client")
        .arg(arg!(<HOST> "server hostname or IP"))
        .arg(
            arg!(<PORT> "server port").value_parser(value_parser!(u16)), // Arg::new("PORT").help("server port").required(true).index(2)
        )
        .arg(
            Arg::new("USERNAME")
                .help("server username")
                .long("username"), // .takes_value(true),
        )
        .arg(
            Arg::new("PASSWORD")
                .help("server password")
                .long("password"), // .takes_value(true),
        )
        .arg(
            Arg::new("EXCLUSIVE")
                .help("request a non-shared session")
                .long("exclusive")
                .action(ArgAction::SetFalse),
        )
        .arg(
            Arg::new("VIEW-ONLY")
                .help("ignore any input")
                .long("view-only")
                .action(ArgAction::SetFalse),
        )
        .arg(
            Arg::new("QEMU-HACKS")
                .help("hack around QEMU/XenHVM's braindead VNC server")
                .long("heinous-qemu-hacks")
                .action(ArgAction::SetFalse),
        )
        .get_matches();

    let host = matches.get_one::<String>("HOST").unwrap();
    let port = matches.get_one::<u16>("PORT").unwrap();
    let username = matches.get_one::<String>("USERNAME");
    let password = matches.get_one::<String>("PASSWORD");
    let exclusive = matches.get_flag("EXCLUSIVE");
    let view_only = matches.get_flag("VIEW-ONLY");
    let qemu_hacks = matches.get_flag("QEMU-HACKS");

    info!("connecting to {}:{}", host, port);
    let stream = match std::net::TcpStream::connect_timeout(
        &format!("{}:{}", host, port).parse().unwrap(),
        // (host.to_owned(), port.to_owned()),
        Duration::from_secs(3),
    ) {
        Ok(stream) => stream,
        Err(error) => {
            error!("cannot connect to {}:{}: {}", host, port, error);
            std::process::exit(1)
        }
    };

    let mut vnc = match t_vnc::Client::from_tcp_stream(stream, !exclusive, |methods| {
        debug!("available authentication methods: {:?}", methods);
        for method in methods {
            match method {
                t_vnc::client::AuthMethod::None => return Some(t_vnc::client::AuthChoice::None),
                t_vnc::client::AuthMethod::Password => {
                    return match password {
                        None => None,
                        Some(password) => {
                            let mut key = [0; 8];
                            for (i, byte) in password.bytes().enumerate() {
                                if i == 8 {
                                    break;
                                }
                                key[i] = byte
                            }
                            Some(t_vnc::client::AuthChoice::Password(key))
                        }
                    }
                }
                t_vnc::client::AuthMethod::AppleRemoteDesktop => match (username, password) {
                    (Some(username), Some(password)) => {
                        return Some(t_vnc::client::AuthChoice::AppleRemoteDesktop(
                            username.to_owned(),
                            password.to_owned(),
                        ))
                    }
                    _ => (),
                },
                _ => (),
            }
        }
        None
    }) {
        Ok(vnc) => vnc,
        Err(error) => {
            error!("cannot initialize VNC session: {}", error);
            std::process::exit(1)
        }
    };

    let (mut width, mut height) = vnc.size();
    info!(
        "connected to \"{}\", {}x{} framebuffer",
        vnc.name(),
        width,
        height
    );

    let mut vnc_format = vnc.format();
    info!("received {:?}", vnc_format);

    let sdl_context = sdl2::init().unwrap();
    let sdl_video = sdl_context.video().unwrap();
    let sdl_timer = sdl_context.timer().unwrap();
    let mut sdl_events = sdl_context.event_pump().unwrap();

    let sdl_format = match pixel_format_vnc_to_sdl(vnc_format) {
        Some(format) => format,
        None => {
            let sdl_format = SdlPixelFormat::RGB888;
            vnc_format = pixel_format_sdl_to_vnc(sdl_format).unwrap();
            warn!(
                "server's natural framebuffer format {:?} is not supported, \
                       using {:?} instead",
                vnc_format, sdl_format
            );
            vnc.set_format(vnc_format).unwrap();
            sdl_format
        }
    };
    info!("rendering to a {:?} texture", sdl_format);

    if qemu_hacks {
        vnc.set_encodings(&[t_vnc::Encoding::Zrle, t_vnc::Encoding::DesktopSize])
            .unwrap()
    } else {
        vnc.set_encodings(&[
            t_vnc::Encoding::Zrle,
            t_vnc::Encoding::CopyRect,
            t_vnc::Encoding::Raw,
            t_vnc::Encoding::Cursor,
            t_vnc::Encoding::DesktopSize,
        ])
        .unwrap()
    }

    let window = sdl_video
        .window(
            &format!("{} - {}:{} - RVNC", vnc.name(), host, port),
            width as u32,
            height as u32,
        )
        .build()
        .unwrap();
    sdl_video.text_input().start();

    let mut canvas = window.into_canvas().build().unwrap();
    let renderer = canvas.texture_creator();
    let mut screen = renderer
        .create_texture_streaming(sdl_format, width as u32, height as u32)
        .unwrap();

    let mut cursor = None;
    let mut cursor_rect = None;
    let (mut hotspot_x, mut hotspot_y) = (0u16, 0u16);

    let mut mouse_buttons = 0u8;
    let (mut mouse_x, mut mouse_y) = (0u16, 0u16);

    let mut key_ctrl = false;

    canvas.clear();
    vnc.request_update(
        t_vnc::Rect {
            left: 0,
            top: 0,
            width,
            height,
        },
        false,
    )
    .unwrap();

    let mut incremental = true;
    let mut qemu_network_rtt = 1000;
    let mut qemu_prev_update = sdl_timer.ticks();
    let mut qemu_next_update = sdl_timer.ticks() + qemu_network_rtt / 2;
    'running: loop {
        const FRAME_MS: u32 = 1000 / 60;
        let ticks = sdl_timer.ticks();

        canvas.present();

        for event in vnc.poll_iter() {
            use t_vnc::client::Event;

            match event {
                Event::Disconnected(None) => break 'running,
                Event::Disconnected(Some(error)) => {
                    error!("server disconnected: {:?}", error);
                    break 'running;
                }
                Event::Resize(new_width, new_height) => {
                    width = new_width;
                    height = new_height;
                    canvas
                        .window_mut()
                        .set_size(width as u32, height as u32)
                        .expect("canvas resize window failed");
                    screen = renderer
                        .create_texture_streaming(sdl_format, width as u32, height as u32)
                        .unwrap();
                    incremental = false;
                }
                Event::PutPixels(vnc_rect, ref pixels) => {
                    let sdl_rect = SdlRect::new(
                        vnc_rect.left as i32,
                        vnc_rect.top as i32,
                        vnc_rect.width as u32,
                        vnc_rect.height as u32,
                    );
                    screen
                        .update(
                            Some(sdl_rect),
                            pixels,
                            sdl_format.byte_size_of_pixels(vnc_rect.width as usize),
                        )
                        .unwrap();
                    canvas
                        .copy(&screen, Some(sdl_rect), Some(sdl_rect))
                        .expect("canvas copy failed");
                    incremental |= vnc_rect
                        == t_vnc::Rect {
                            left: 0,
                            top: 0,
                            width,
                            height,
                        };
                }
                Event::CopyPixels {
                    src: vnc_src,
                    dst: vnc_dst,
                } => {
                    let sdl_src = SdlRect::new(
                        vnc_src.left as i32,
                        vnc_src.top as i32,
                        vnc_src.width as u32,
                        vnc_src.height as u32,
                    );
                    let sdl_dst = SdlRect::new(
                        vnc_dst.left as i32,
                        vnc_dst.top as i32,
                        vnc_dst.width as u32,
                        vnc_dst.height as u32,
                    );
                    let pixels = canvas.read_pixels(Some(sdl_src), sdl_format).unwrap();
                    screen
                        .update(
                            Some(sdl_dst),
                            &pixels,
                            sdl_format.byte_size_of_pixels(vnc_dst.width as usize),
                        )
                        .unwrap();
                    canvas
                        .copy(&screen, Some(sdl_dst), Some(sdl_dst))
                        .expect("canvas copy failed");
                }
                Event::EndOfFrame => {
                    if qemu_hacks {
                        let network_rtt = sdl_timer.ticks() - qemu_prev_update;
                        // qemu_network_rtt = network_rtt;
                        qemu_network_rtt = qemu_network_rtt * 80 / 100 + network_rtt * 20 / 100;
                        qemu_prev_update = sdl_timer.ticks();
                        qemu_next_update = sdl_timer.ticks() + qemu_network_rtt / 2;
                        debug!("network RTT: {} ms", qemu_network_rtt);
                    }
                }
                Event::Clipboard(ref text) => {
                    let _ = sdl_video.clipboard().set_clipboard_text(text);
                    // this returns a Result, but unwrapping it fails with "Invalid renderer",
                    // even though the call to set_clipboard_text actually succeeds.
                }
                Event::SetCursor {
                    size: (width, height),
                    hotspot: (new_hotspot_x, new_hotspot_y),
                    pixels,
                    mask_bits,
                } => {
                    hotspot_x = new_hotspot_x;
                    hotspot_y = new_hotspot_y;
                    if width > 0 && height > 0 {
                        let mut mask_pixels = Vec::new();
                        let mask_stride = (width + 7) / 8;
                        for y in 0..height {
                            for x in 0..mask_stride {
                                let mask_byte = mask_bits[(y * mask_stride + x) as usize];
                                for w in 0..8 {
                                    mask_pixels.push(mask_byte & (1 << (7 - w)))
                                }
                            }
                        }
                        let (sdl_cursor_format, cursor_pixels) =
                            mask_cursor(vnc_format, pixels, mask_pixels);
                        let mut new_cursor = renderer
                            .create_texture_streaming(
                                sdl_cursor_format,
                                width as u32,
                                height as u32,
                            )
                            .unwrap();
                        new_cursor
                            .update(
                                None,
                                &cursor_pixels,
                                sdl_cursor_format.byte_size_of_pixels(width as usize),
                            )
                            .unwrap();
                        new_cursor.set_blend_mode(sdl2::render::BlendMode::Blend);
                        cursor = Some(new_cursor);
                    } else {
                        cursor = None
                    }
                }
                _ => (), /* ignore unsupported events */
            }

            if sdl_timer.ticks() - ticks > FRAME_MS {
                continue 'running;
            }
        }

        if let Some(cursor_rect) = cursor_rect {
            canvas
                .copy(&screen, Some(cursor_rect), Some(cursor_rect))
                .expect("canvas copy failed");
        }

        match cursor {
            Some(ref cursor) => {
                sdl_context.mouse().show_cursor(false);

                let raw_cursor_rect = SdlRect::new(
                    mouse_x as i32 - hotspot_x as i32,
                    mouse_y as i32 - hotspot_y as i32,
                    cursor.query().width,
                    cursor.query().height,
                );
                let screen_rect = SdlRect::new(0, 0, width as u32, height as u32);
                let clipped_cursor_rect = raw_cursor_rect & screen_rect;
                if let Some(clipped_cursor_rect) = clipped_cursor_rect {
                    let source_rect = SdlRect::new(
                        clipped_cursor_rect.x() - raw_cursor_rect.x(),
                        clipped_cursor_rect.y() - raw_cursor_rect.y(),
                        clipped_cursor_rect.width(),
                        clipped_cursor_rect.height(),
                    );
                    canvas
                        .copy(cursor, Some(source_rect), Some(clipped_cursor_rect))
                        .expect("canvas copy failed");
                }
                cursor_rect = clipped_cursor_rect;
            }
            None => {
                sdl_context.mouse().show_cursor(true);

                cursor_rect = None;
            }
        }

        for event in sdl_events.wait_timeout_iter(sdl_timer.ticks() - ticks + FRAME_MS) {
            use sdl2::event::{Event, WindowEvent};

            match event {
                Event::Quit { .. } => break 'running,
                Event::Window {
                    win_event: WindowEvent::SizeChanged(width, height),
                    ..
                } => {
                    let screen_rect = SdlRect::new(0, 0, width as u32, height as u32);
                    canvas
                        .copy(&screen, None, Some(screen_rect))
                        .expect("canvas copy failed");
                    canvas.present()
                }
                _ => (),
            }

            if view_only {
                continue;
            }

            match event {
                Event::KeyDown {
                    keycode: Some(keycode),
                    ..
                }
                | Event::KeyUp {
                    keycode: Some(keycode),
                    ..
                } => {
                    use sdl2::keyboard::Keycode;
                    let down = matches!(event, Event::KeyDown { .. });
                    match keycode {
                        Keycode::LCtrl | Keycode::RCtrl => key_ctrl = down,
                        _ => (),
                    }
                    if let Some(keysym) = map_special_key(key_ctrl, keycode) {
                        vnc.send_key_event(down, keysym).unwrap();
                    }
                }
                Event::TextInput { text, .. } => {
                    let chr = 0x01000000 + text.chars().next().unwrap() as u32;
                    vnc.send_key_event(true, chr).unwrap();
                    vnc.send_key_event(false, chr).unwrap()
                }
                Event::MouseMotion { x, y, .. } => {
                    mouse_x = x as u16;
                    mouse_y = y as u16;
                    if !qemu_hacks {
                        vnc.send_pointer_event(mouse_buttons, mouse_x, mouse_y)
                            .unwrap()
                    }
                }
                Event::MouseButtonDown {
                    x, y, mouse_btn, ..
                }
                | Event::MouseButtonUp {
                    x, y, mouse_btn, ..
                } => {
                    use sdl2::mouse::MouseButton;
                    mouse_x = x as u16;
                    mouse_y = y as u16;
                    let mouse_button = match mouse_btn {
                        MouseButton::Left => 0x01,
                        MouseButton::Middle => 0x02,
                        MouseButton::Right => 0x04,
                        MouseButton::X1 => 0x20,
                        MouseButton::X2 => 0x40,
                        MouseButton::Unknown => 0x00,
                    };
                    match event {
                        Event::MouseButtonDown { .. } => mouse_buttons |= mouse_button,
                        Event::MouseButtonUp { .. } => mouse_buttons &= !mouse_button,
                        _ => unreachable!(),
                    };
                    vnc.send_pointer_event(mouse_buttons, mouse_x, mouse_y)
                        .unwrap()
                }
                Event::MouseWheel { y, .. } => {
                    if y == 1 {
                        vnc.send_pointer_event(mouse_buttons | 0x08, mouse_x, mouse_y)
                            .unwrap();
                        vnc.send_pointer_event(mouse_buttons, mouse_x, mouse_y)
                            .unwrap();
                    } else if y == -1 {
                        vnc.send_pointer_event(mouse_buttons | 0x10, mouse_x, mouse_y)
                            .unwrap();
                        vnc.send_pointer_event(mouse_buttons, mouse_x, mouse_y)
                            .unwrap();
                    }
                }
                Event::ClipboardUpdate { .. } => vnc
                    .update_clipboard(&sdl_video.clipboard().clipboard_text().unwrap())
                    .unwrap(),
                _ => (),
            }
        }

        if qemu_hacks && sdl_timer.ticks() > qemu_next_update {
            // QEMU ignores incremental update requests and sends non-incremental ones,
            // but does not update framebuffer in them. However, it does update framebuffer
            // (and send it to us) if we change the pixel format, including not actually
            // changing it.
            vnc.poke_qemu().unwrap();
            qemu_next_update = sdl_timer.ticks() + qemu_network_rtt / 2;
        } else {
            vnc.request_update(
                t_vnc::Rect {
                    left: 0,
                    top: 0,
                    width,
                    height,
                },
                incremental,
            )
            .unwrap();
        }
    }
}

fn map_special_key(alnum_ok: bool, keycode: sdl2::keyboard::Keycode) -> Option<u32> {
    use sdl2::keyboard::Keycode::*;
    use x11::keysym::*;

    let x11code = match keycode {
        Space => XK_space,
        Exclaim => XK_exclam,
        Quotedbl => XK_quotedbl,
        Hash => XK_numbersign,
        Dollar => XK_dollar,
        Percent => XK_percent,
        Ampersand => XK_ampersand,
        Quote => XK_apostrophe,
        LeftParen => XK_parenleft,
        RightParen => XK_parenright,
        Asterisk => XK_asterisk,
        Plus => XK_plus,
        Comma => XK_comma,
        Minus => XK_minus,
        Period => XK_period,
        Slash => XK_slash,
        Num0 => XK_0,
        Num1 => XK_1,
        Num2 => XK_2,
        Num3 => XK_3,
        Num4 => XK_4,
        Num5 => XK_5,
        Num6 => XK_6,
        Num7 => XK_7,
        Num8 => XK_8,
        Num9 => XK_9,
        Colon => XK_colon,
        Semicolon => XK_semicolon,
        Less => XK_less,
        Equals => XK_equal,
        Greater => XK_greater,
        Question => XK_question,
        At => XK_at,
        LeftBracket => XK_bracketleft,
        Backslash => XK_backslash,
        RightBracket => XK_bracketright,
        Caret => XK_caret,
        Underscore => XK_underscore,
        Backquote => XK_grave,
        A => XK_a,
        B => XK_b,
        C => XK_c,
        D => XK_d,
        E => XK_e,
        F => XK_f,
        G => XK_g,
        H => XK_h,
        I => XK_i,
        J => XK_j,
        K => XK_k,
        L => XK_l,
        M => XK_m,
        N => XK_n,
        O => XK_o,
        P => XK_p,
        Q => XK_q,
        R => XK_r,
        S => XK_s,
        T => XK_t,
        U => XK_u,
        V => XK_v,
        W => XK_w,
        X => XK_x,
        Y => XK_y,
        Z => XK_z,
        _ => 0,
    };
    if x11code != 0 && alnum_ok {
        return Some(x11code);
    }

    let x11code = match keycode {
        Backspace => XK_BackSpace,
        Tab => XK_Tab,
        Return => XK_Return,
        Escape => XK_Escape,
        Delete => XK_Delete,
        CapsLock => XK_Caps_Lock,
        F1 => XK_F1,
        F2 => XK_F2,
        F3 => XK_F3,
        F4 => XK_F4,
        F5 => XK_F5,
        F6 => XK_F6,
        F7 => XK_F7,
        F8 => XK_F8,
        F9 => XK_F9,
        F10 => XK_F10,
        F11 => XK_F11,
        F12 => XK_F12,
        PrintScreen => XK_Print,
        ScrollLock => XK_Scroll_Lock,
        Pause => XK_Pause,
        Insert => XK_Insert,
        Home => XK_Home,
        PageUp => XK_Page_Up,
        End => XK_End,
        PageDown => XK_Page_Down,
        Right => XK_Right,
        Left => XK_Left,
        Down => XK_Down,
        Up => XK_Up,
        NumLockClear => XK_Num_Lock,
        KpDivide => XK_KP_Divide,
        KpMultiply => XK_KP_Multiply,
        KpMinus => XK_KP_Subtract,
        KpPlus => XK_KP_Add,
        KpEnter => XK_KP_Enter,
        Kp1 => XK_KP_1,
        Kp2 => XK_KP_2,
        Kp3 => XK_KP_3,
        Kp4 => XK_KP_4,
        Kp5 => XK_KP_5,
        Kp6 => XK_KP_6,
        Kp7 => XK_KP_7,
        Kp8 => XK_KP_8,
        Kp9 => XK_KP_9,
        Kp0 => XK_KP_0,
        KpPeriod => XK_KP_Separator,
        F13 => XK_F13,
        F14 => XK_F14,
        F15 => XK_F15,
        F16 => XK_F16,
        F17 => XK_F17,
        F18 => XK_F18,
        F19 => XK_F19,
        F20 => XK_F20,
        F21 => XK_F21,
        F22 => XK_F22,
        F23 => XK_F23,
        F24 => XK_F24,
        Menu => XK_Menu,
        Sysreq => XK_Sys_Req,
        LCtrl => XK_Control_L,
        LShift => XK_Shift_L,
        LAlt => XK_Alt_L,
        LGui => XK_Super_L,
        RCtrl => XK_Control_R,
        RShift => XK_Shift_R,
        RAlt => XK_Alt_R,
        RGui => XK_Super_R,
        _ => 0,
    };
    if x11code != 0 {
        Some(x11code)
    } else {
        None
    }
}
