mod rfb;

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;
use rfb::{Connection, Framebuffer, Sender};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    // `vnc-viewer host[:port]` connects straight away (useful for servers without a password).
    let host = std::env::args().nth(1);
    eframe::run_native(
        "VNC Viewer",
        options,
        Box::new(|cc| {
            let mut app = App::default();
            if let Some(host) = host {
                app.host = host;
                app.connect(&cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}

enum Event {
    Connected(Arc<Mutex<Framebuffer>>, Sender),
    Closed(String),
}

#[derive(Default)]
struct Session {
    fb: Option<Arc<Mutex<Framebuffer>>>,
    sender: Option<Sender>,
    texture: Option<egui::TextureHandle>,
    last_pointer: Option<(u8, u16, u16)>,
    modifiers: egui::Modifiers,
}

struct App {
    host: String,
    password: String,
    status: String,
    events: Option<Receiver<Event>>,
    session: Session,
}

impl Default for App {
    fn default() -> Self {
        Self {
            host: "localhost:5900".into(),
            password: String::new(),
            status: String::new(),
            events: None,
            session: Session::default(),
        }
    }
}

impl App {
    fn connect(&mut self, ctx: &egui::Context) {
        let addr = if self.host.contains(':') {
            self.host.clone()
        } else {
            format!("{}:5900", self.host)
        };
        let password = self.password.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = Connection::connect(&addr, &password).and_then(|conn| {
                let _ = tx.send(Event::Connected(conn.fb.clone(), conn.sender.clone()));
                ctx.request_repaint();
                let repaint = ctx.clone();
                conn.run(move || repaint.request_repaint())
            });
            let msg = match result {
                Ok(()) => "Disconnected".into(),
                Err(e) => format!("Disconnected: {e:#}"),
            };
            let _ = tx.send(Event::Closed(msg));
            ctx.request_repaint();
        });
        self.events = Some(rx);
        self.status = "Connecting...".into();
    }

    fn poll_events(&mut self) {
        let Some(rx) = &self.events else { return };
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Connected(fb, sender) => {
                    self.status = format!("Connected: {}", fb.lock().unwrap().name);
                    self.session = Session {
                        fb: Some(fb),
                        sender: Some(sender),
                        ..Default::default()
                    };
                }
                Event::Closed(msg) => {
                    self.status = msg;
                    self.session = Session::default();
                }
            }
        }
    }

    fn login_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VNC Viewer");
            egui::Grid::new("login").num_columns(2).show(ui, |ui| {
                ui.label("Server");
                ui.text_edit_singleline(&mut self.host);
                ui.end_row();
                ui.label("Password");
                let pw = ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.end_row();
                let enter = pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Connect").clicked() || enter {
                    self.connect(ctx);
                }
            });
            ui.label(&self.status);
        });
    }

    fn remote_ui(&mut self, ctx: &egui::Context) {
        let s = &mut self.session;
        let (Some(fb), Some(sender)) = (s.fb.clone(), s.sender.clone()) else {
            return;
        };

        {
            let mut fb = fb.lock().unwrap();
            if fb.dirty || s.texture.is_none() {
                let image =
                    egui::ColorImage::from_rgba_unmultiplied([fb.width, fb.height], &fb.pixels);
                match &mut s.texture {
                    Some(t) if t.size() == [fb.width, fb.height] => {
                        t.set(image, egui::TextureOptions::LINEAR)
                    }
                    _ => {
                        s.texture =
                            Some(ctx.load_texture("remote", image, egui::TextureOptions::LINEAR))
                    }
                }
                fb.dirty = false;
            }
        }
        let Some(texture) = &s.texture else { return };

        egui::TopBottomPanel::top("status").show(ctx, |ui| ui.label(&self.status));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let resp = ui.add(
                    egui::Image::new(texture)
                        .shrink_to_fit()
                        .sense(egui::Sense::click_and_drag()),
                );
                let [fw, fh] = texture.size();
                let rect = resp.rect;
                let to_fb = |p: egui::Pos2| {
                    let x =
                        ((p.x - rect.min.x) / rect.width() * fw as f32).clamp(0.0, fw as f32 - 1.0);
                    let y = ((p.y - rect.min.y) / rect.height() * fh as f32)
                        .clamp(0.0, fh as f32 - 1.0);
                    (x as u16, y as u16)
                };

                ctx.input(|i| {
                    // Pointer
                    if let Some(pos) = i.pointer.latest_pos().filter(|p| rect.contains(*p)) {
                        let (x, y) = to_fb(pos);
                        let mut buttons = 0u8;
                        if i.pointer.primary_down() {
                            buttons |= 1;
                        }
                        if i.pointer.middle_down() {
                            buttons |= 2;
                        }
                        if i.pointer.secondary_down() {
                            buttons |= 4;
                        }
                        if s.last_pointer != Some((buttons, x, y)) {
                            sender.pointer(buttons, x, y);
                            s.last_pointer = Some((buttons, x, y));
                        }
                        for ev in &i.events {
                            if let egui::Event::MouseWheel { delta, .. } = ev {
                                let wheel = if delta.y > 0.0 {
                                    8
                                } else if delta.y < 0.0 {
                                    16
                                } else {
                                    0
                                };
                                if wheel != 0 {
                                    sender.pointer(buttons | wheel, x, y);
                                    sender.pointer(buttons, x, y);
                                }
                            }
                        }
                    }

                    // Modifiers
                    for (was, now, keysym) in [
                        (s.modifiers.shift, i.modifiers.shift, 0xffe1),
                        (s.modifiers.ctrl, i.modifiers.ctrl, 0xffe3),
                        (s.modifiers.alt, i.modifiers.alt, 0xffe9),
                    ] {
                        if was != now {
                            sender.key(keysym, now);
                        }
                    }
                    s.modifiers = i.modifiers;

                    // Keys
                    for ev in &i.events {
                        match ev {
                            egui::Event::Text(text) => {
                                for c in text.chars() {
                                    let sym = char_keysym(c);
                                    sender.key(sym, true);
                                    sender.key(sym, false);
                                }
                            }
                            egui::Event::Copy => tap(&sender, 'c' as u32),
                            egui::Event::Cut => tap(&sender, 'x' as u32),
                            egui::Event::Paste(_) => tap(&sender, 'v' as u32),
                            egui::Event::Key {
                                key,
                                pressed,
                                modifiers,
                                ..
                            } => {
                                if let Some(sym) = special_keysym(*key) {
                                    sender.key(sym, *pressed);
                                } else if modifiers.ctrl || modifiers.alt {
                                    // Text events are not generated while Ctrl/Alt is held.
                                    if let Some(c) =
                                        key.name().chars().next().filter(|_| key.name().len() == 1)
                                    {
                                        sender.key(c.to_ascii_lowercase() as u32, *pressed);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                });
            });
    }
}

fn tap(sender: &Sender, keysym: u32) {
    sender.key(keysym, true);
    sender.key(keysym, false);
}

fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
        cp
    } else {
        0x0100_0000 + cp
    }
}

fn special_keysym(key: egui::Key) -> Option<u32> {
    use egui::Key::*;
    Some(match key {
        Backspace => 0xff08,
        Tab => 0xff09,
        Enter => 0xff0d,
        Escape => 0xff1b,
        Insert => 0xff63,
        Delete => 0xffff,
        Home => 0xff50,
        End => 0xff57,
        PageUp => 0xff55,
        PageDown => 0xff56,
        ArrowLeft => 0xff51,
        ArrowUp => 0xff52,
        ArrowRight => 0xff53,
        ArrowDown => 0xff54,
        F1 => 0xffbe,
        F2 => 0xffbf,
        F3 => 0xffc0,
        F4 => 0xffc1,
        F5 => 0xffc2,
        F6 => 0xffc3,
        F7 => 0xffc4,
        F8 => 0xffc5,
        F9 => 0xffc6,
        F10 => 0xffc7,
        F11 => 0xffc8,
        F12 => 0xffc9,
        _ => return None,
    })
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        if self.session.fb.is_some() {
            self.remote_ui(ctx);
        } else {
            self.login_ui(ctx);
        }
    }
}
