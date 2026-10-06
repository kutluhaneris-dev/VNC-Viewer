//! The viewer UI: home screen with saved connections, dialogs, and the remote session view.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Sense, Stroke, Vec2, vec2};

use crate::keys;
use crate::rfb::{AuthError, Connection, Framebuffer, Sender};
use crate::store::{self, SavedConnection, Store};

const ACCENT: Color32 = Color32::from_rgb(0x3b, 0x82, 0xf6);
const BG: Color32 = Color32::from_rgb(0x16, 0x18, 0x1d);
const SURFACE: Color32 = Color32::from_rgb(0x20, 0x23, 0x2a);
const SURFACE_HOVER: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x37);
const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xad);
const DANGER: Color32 = Color32::from_rgb(0xef, 0x44, 0x44);

/// What we are connecting to.
#[derive(Clone)]
struct Target {
    title: String,
    address: String,
    password: String,
    view_only: bool,
    saved_id: Option<u64>,
    /// Save the password on success (ticked in the password prompt).
    remember_password: bool,
}

enum NetEvent {
    Connected(Arc<Mutex<Framebuffer>>, Sender),
    NeedPassword,
    WrongPassword(String),
    Closed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Scale {
    Fit,
    Native,
}

struct Session {
    target: Target,
    fb: Arc<Mutex<Framebuffer>>,
    sender: Sender,
    rx: Receiver<NetEvent>,
    texture: Option<egui::TextureHandle>,
    last_pointer: Option<(u8, u16, u16)>,
    modifiers: egui::Modifiers,
    scale: Scale,
    fullscreen: bool,
    toolbar_hovered: bool,
}

enum State {
    Home,
    Connecting {
        target: Target,
        rx: Receiver<NetEvent>,
    },
    Connected(Box<Session>),
}

struct Editor {
    conn: SavedConnection,
    focus: bool,
    password: String,
    remember: bool,
}

struct PasswordPrompt {
    target: Target,
    error: Option<String>,
    focus: bool,
}

pub struct App {
    store: Store,
    query: String,
    state: State,
    error: Option<String>,
    editor: Option<Editor>,
    prompt: Option<PasswordPrompt>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, host: Option<String>) -> Self {
        apply_theme(&cc.egui_ctx);
        let store = cc
            .storage
            .and_then(|s| eframe::get_value(s, Store::KEY))
            .unwrap_or_default();
        let mut app = Self {
            store,
            query: String::new(),
            state: State::Home,
            error: None,
            editor: None,
            prompt: None,
        };
        if let Some(host) = host {
            let target = app.target_for_query(&host);
            app.connect(&cc.egui_ctx, target);
        }
        app
    }

    fn target_from_saved(c: &SavedConnection) -> Target {
        Target {
            title: c.title().to_string(),
            address: store::normalize_address(&c.address),
            password: c.password.clone().unwrap_or_default(),
            view_only: c.view_only,
            saved_id: Some(c.id),
            remember_password: c.password.is_some(),
        }
    }

    /// A saved connection whose name or address matches exactly, else an ad-hoc target.
    fn target_for_query(&self, q: &str) -> Target {
        let q = q.trim();
        let found = self.store.connections.iter().find(|c| {
            c.name.eq_ignore_ascii_case(q)
                || store::normalize_address(&c.address) == store::normalize_address(q)
        });
        match found {
            Some(c) => Self::target_from_saved(c),
            None => Target {
                title: q.to_string(),
                address: store::normalize_address(q),
                password: String::new(),
                view_only: false,
                saved_id: None,
                remember_password: false,
            },
        }
    }

    fn connect(&mut self, ctx: &egui::Context, target: Target) {
        self.error = None;
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let (addr, password) = (target.address.clone(), target.password.clone());
        std::thread::spawn(move || {
            let ev = match Connection::connect(&addr, &password) {
                Ok(conn) => {
                    if tx
                        .send(NetEvent::Connected(conn.fb.clone(), conn.sender.clone()))
                        .is_err()
                    {
                        return; // cancelled while connecting
                    }
                    ctx2.request_repaint();
                    let repaint = ctx2.clone();
                    match conn.run(move || repaint.request_repaint()) {
                        Ok(()) => NetEvent::Closed("Sunucu bağlantıyı kapattı.".into()),
                        Err(e) => NetEvent::Closed(format!("Bağlantı koptu: {}", explain(&e))),
                    }
                }
                Err(e) => match e.downcast_ref::<AuthError>() {
                    Some(AuthError::PasswordRequired) => NetEvent::NeedPassword,
                    Some(AuthError::Rejected(r)) => NetEvent::WrongPassword(r.clone()),
                    None => NetEvent::Closed(format!("Bağlanılamadı: {}", explain(&e))),
                },
            };
            let _ = tx.send(ev);
            ctx2.request_repaint();
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{} – VNC Viewer",
            target.title
        )));
        self.state = State::Connecting { target, rx };
    }

    fn go_home(&mut self, ctx: &egui::Context, error: Option<String>) {
        if let State::Connected(s) = &self.state {
            s.sender.shutdown();
            if s.fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }
        self.state = State::Home;
        self.error = error;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("VNC Viewer".into()));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        match &mut self.state {
            State::Home => {}
            State::Connecting { target, rx } => {
                let Ok(ev) = rx.try_recv() else { return };
                let target = target.clone();
                match ev {
                    NetEvent::Connected(fb, sender) => {
                        let rx = std::mem::replace(rx, channel().1);
                        self.on_connected(&target);
                        self.state = State::Connected(Box::new(Session {
                            target,
                            fb,
                            sender,
                            rx,
                            texture: None,
                            last_pointer: None,
                            modifiers: Default::default(),
                            scale: Scale::Fit,
                            fullscreen: false,
                            toolbar_hovered: false,
                        }));
                    }
                    NetEvent::NeedPassword => {
                        self.go_home(ctx, None);
                        self.prompt = Some(PasswordPrompt {
                            target,
                            error: None,
                            focus: true,
                        });
                    }
                    NetEvent::WrongPassword(reason) => {
                        self.go_home(ctx, None);
                        let mut target = target;
                        target.password.clear();
                        self.prompt = Some(PasswordPrompt {
                            target,
                            error: Some(format!("Şifre kabul edilmedi ({reason}).")),
                            focus: true,
                        });
                    }
                    NetEvent::Closed(msg) => self.go_home(ctx, Some(msg)),
                }
            }
            State::Connected(s) => {
                if let Ok(NetEvent::Closed(msg)) = s.rx.try_recv() {
                    self.go_home(ctx, Some(msg));
                }
            }
        }
    }

    fn on_connected(&mut self, t: &Target) {
        let id = match t.saved_id {
            Some(id) => id,
            // Remembering a password for an ad-hoc address saves it as a connection.
            None if t.remember_password => self.store.upsert(SavedConnection {
                address: t.address.clone(),
                ..Default::default()
            }),
            None => return,
        };
        if let Some(c) = self.store.connections.iter_mut().find(|c| c.id == id) {
            if t.remember_password {
                c.password = Some(t.password.clone());
            }
        }
        self.store.touch(id);
    }

    // ---------------------------------------------------------------- home

    fn home_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(24, 16)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("VNC Viewer").size(22.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_button("+  Yeni bağlantı")).clicked() {
                            self.editor = Some(Editor {
                                focus: true,
                                conn: SavedConnection::default(),
                                password: String::new(),
                                remember: false,
                            });
                        }
                    });
                });
            });

        egui::TopBottomPanel::bottom("footer")
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(24, 10)))
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Hesap yok · lisans yok · güncelleme yok. Bağlantılar yalnızca bu bilgisayarda saklanır.")
                        .size(12.0)
                        .color(MUTED),
                );
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(24, 8)))
            .show(ctx, |ui| {
                // Address / search bar
                let bar = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Adres girin (ör. 192.168.1.10 veya pc:5901) ya da kayıtlı bağlantılarda arayın")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(14, 10))
                        .font(egui::TextStyle::Body),
                );
                if bar.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.query.trim().is_empty() {
                    let target = self.target_for_query(&self.query);
                    self.connect(ctx, target);
                }

                if let Some(err) = self.error.clone() {
                    ui.add_space(12.0);
                    egui::Frame::new()
                        .fill(DANGER.gamma_multiply(0.15))
                        .stroke(Stroke::new(1.0_f32, DANGER.gamma_multiply(0.6)))
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(err).color(Color32::from_rgb(0xfc, 0xa5, 0xa5)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("×").clicked() {
                                        self.error = None;
                                    }
                                });
                            });
                        });
                }

                ui.add_space(20.0);
                ui.label(RichText::new("Bağlantılar").size(15.0).color(MUTED));
                ui.add_space(8.0);

                let q = self.query.trim().to_lowercase();
                let items: Vec<SavedConnection> = self
                    .store
                    .sorted()
                    .into_iter()
                    .filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q) || c.address.to_lowercase().contains(&q))
                    .cloned()
                    .collect();

                if items.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        let text = if self.store.connections.is_empty() {
                            "Henüz kayıtlı bağlantı yok.\nYukarıya bir adres yazıp Enter'a basın ya da “Yeni bağlantı” ekleyin."
                        } else {
                            "Aramayla eşleşen bağlantı yok. Enter'a basarak bu adrese bağlanabilirsiniz."
                        };
                        ui.label(RichText::new(text).color(MUTED));
                    });
                    return;
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
                        for c in &items {
                            self.card(ui, ctx, c);
                        }
                    });
                });
            });
    }

    fn card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, c: &SavedConnection) {
        let size = vec2(250.0, 96.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let hovered = resp.hovered();
        let painter = ui.painter_at(rect);
        painter.rect(
            rect,
            CornerRadius::same(10),
            if hovered { SURFACE_HOVER } else { SURFACE },
            Stroke::new(
                1.0_f32,
                if hovered {
                    ACCENT.gamma_multiply(0.7)
                } else {
                    Color32::from_gray(48)
                },
            ),
            egui::StrokeKind::Inside,
        );

        // Monitor icon
        let icon = egui::Rect::from_min_size(rect.min + vec2(16.0, 22.0), vec2(40.0, 28.0));
        painter.rect_filled(icon, CornerRadius::same(4), ACCENT.gamma_multiply(0.85));
        painter.rect_filled(
            icon.shrink(3.0),
            CornerRadius::same(2),
            Color32::from_rgb(0x1e, 0x3a, 0x8a),
        );
        painter.rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(icon.center().x, icon.max.y + 5.0),
                vec2(16.0, 3.0),
            ),
            CornerRadius::same(1),
            ACCENT.gamma_multiply(0.85),
        );

        let text_x = rect.min.x + 72.0;
        let max_w = rect.max.x - text_x - 12.0;
        let fit = |s: &str, size: f32, color: Color32| {
            painter.layout(
                s.to_string(),
                egui::FontId::proportional(size),
                color,
                max_w,
            )
        };
        let title = fit(c.title(), 16.0, Color32::from_gray(235));
        painter.galley(egui::pos2(text_x, rect.min.y + 18.0), title, Color32::WHITE);
        painter.galley(
            egui::pos2(text_x, rect.min.y + 42.0),
            fit(&c.address, 13.0, MUTED),
            MUTED,
        );
        let sub = match c.last_used {
            Some(t) => format!("Son bağlantı: {}", store::ago(t)),
            None => "Hiç bağlanılmadı".into(),
        };
        let sub = if c.view_only {
            format!("{sub} · sadece izle")
        } else {
            sub
        };
        painter.galley(
            egui::pos2(text_x, rect.min.y + 64.0),
            fit(&sub, 11.5, MUTED.gamma_multiply(0.8)),
            MUTED,
        );

        if hovered {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let resp = resp.on_hover_text("Bağlanmak için tıklayın · sağ tık: düzenle / sil");
        if resp.clicked() {
            self.connect(ctx, Self::target_from_saved(c));
        }
        resp.context_menu(|ui| {
            if ui.button("Bağlan").clicked() {
                self.connect(ctx, Self::target_from_saved(c));
                ui.close();
            }
            if ui.button("Düzenle…").clicked() {
                self.editor = Some(Editor {
                    focus: true,
                    conn: c.clone(),
                    password: c.password.clone().unwrap_or_default(),
                    remember: c.password.is_some(),
                });
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Sil").color(DANGER)).clicked() {
                self.store.remove(c.id);
                ui.close();
            }
        });
    }

    fn editor_ui(&mut self, ctx: &egui::Context) {
        let Some(ed) = &mut self.editor else { return };
        let mut close = false;
        let mut save = false;
        let is_new = ed.conn.id == 0;
        let modal = egui::Modal::new(egui::Id::new("editor")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(
                RichText::new(if is_new {
                    "Yeni bağlantı"
                } else {
                    "Bağlantıyı düzenle"
                })
                .size(18.0)
                .strong(),
            );
            ui.add_space(12.0);
            egui::Grid::new("editor-grid")
                .num_columns(2)
                .spacing(vec2(12.0, 10.0))
                .show(ui, |ui| {
                    ui.label("Ad");
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut ed.conn.name)
                            .hint_text("ör. Ofis bilgisayarı")
                            .desired_width(260.0),
                    );
                    if ed.focus {
                        name.request_focus();
                        ed.focus = false;
                    }
                    ui.end_row();
                    ui.label("Adres");
                    ui.add(
                        egui::TextEdit::singleline(&mut ed.conn.address)
                            .hint_text("192.168.1.10:5900")
                            .desired_width(260.0),
                    );
                    ui.end_row();
                    ui.label("Şifre");
                    ui.add(
                        egui::TextEdit::singleline(&mut ed.password)
                            .password(true)
                            .desired_width(260.0),
                    );
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut ed.remember, "Şifreyi hatırla");
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(
                        &mut ed.conn.view_only,
                        "Sadece izle (fare ve klavye gönderme)",
                    );
                    ui.end_row();
                });
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let valid = !ed.conn.address.trim().is_empty();
                if ui.add_enabled(valid, primary_button("Kaydet")).clicked() {
                    save = true;
                }
                if ui.button("İptal").clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if save {
            ed.conn.address = ed.conn.address.trim().to_string();
            ed.conn.password =
                (ed.remember && !ed.password.is_empty()).then(|| ed.password.clone());
            self.store.upsert(ed.conn.clone());
            close = true;
        }
        if close {
            self.editor = None;
        }
    }

    fn prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.prompt else { return };
        let mut go = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("password")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("Şifre gerekli").size(18.0).strong());
            ui.label(RichText::new(&p.target.title).color(MUTED));
            ui.add_space(10.0);
            if let Some(err) = &p.error {
                ui.label(RichText::new(err).color(Color32::from_rgb(0xfc, 0xa5, 0xa5)));
                ui.add_space(4.0);
            }
            let field = ui.add(
                egui::TextEdit::singleline(&mut p.target.password)
                    .password(true)
                    .hint_text("VNC şifresi")
                    .desired_width(f32::INFINITY),
            );
            if p.focus {
                field.request_focus();
                p.focus = false;
            }
            ui.checkbox(&mut p.target.remember_password, "Şifreyi hatırla");
            ui.add_space(10.0);
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if ui.add(primary_button("Bağlan")).clicked() || enter {
                    go = Some(p.target.clone());
                }
                if ui.button("İptal").clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some(t) = go {
            self.prompt = None;
            self.connect(ctx, t);
        } else if close {
            self.prompt = None;
        }
    }

    fn connecting_ui(&mut self, ctx: &egui::Context) {
        let State::Connecting { target, .. } = &self.state else {
            return;
        };
        let title = target.title.clone();
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("connecting")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(20.0));
                ui.label(RichText::new(format!("{title} bağlanılıyor…")).size(15.0));
            });
            ui.add_space(10.0);
            if ui.button("İptal").clicked() {
                cancel = true;
            }
        });
        if cancel {
            self.go_home(ctx, None);
        }
    }

    // ------------------------------------------------------------- session

    fn session_ui(&mut self, ctx: &egui::Context) {
        let mut disconnect = false;
        let State::Connected(s) = &mut self.state else {
            return;
        };

        // Upload changed pixels.
        {
            let mut fb = s.fb.lock().unwrap();
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

        if ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::CTRL | egui::Modifiers::ALT,
                egui::Key::Enter,
            )
        }) {
            toggle_fullscreen(ctx, s);
        }

        // In full screen the toolbar floats over the picture and only appears
        // when the mouse touches the top edge.
        let near_top = ctx.input(|i| i.pointer.latest_pos().is_some_and(|p| p.y < 6.0));
        if !s.fullscreen {
            egui::TopBottomPanel::top("toolbar")
                .frame(
                    egui::Frame::new()
                        .fill(SURFACE)
                        .inner_margin(Margin::symmetric(12, 6)),
                )
                .show(ctx, |ui| toolbar(ui, ctx, s, &mut disconnect));
            s.toolbar_hovered = false;
        } else if near_top || s.toolbar_hovered {
            let width = ctx.screen_rect().width();
            let area = egui::Area::new(egui::Id::new("fullscreen-toolbar"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(SURFACE.gamma_multiply(0.95))
                        .inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.set_width(width - 24.0);
                            toolbar(ui, ctx, s, &mut disconnect);
                        });
                });
            s.toolbar_hovered = area.response.contains_pointer();
        } else {
            s.toolbar_hovered = false;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let Some(texture) = s.texture.clone() else {
                    return;
                };
                let [fw, fh] = texture.size();
                let tex_size = vec2(fw as f32, fh as f32);
                let draw = |ui: &mut egui::Ui, rect: egui::Rect| {
                    ui.painter().image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                };
                let (rect, resp) = match s.scale {
                    Scale::Fit => {
                        let avail = ui.available_rect_before_wrap();
                        let k = (avail.width() / tex_size.x).min(avail.height() / tex_size.y);
                        let rect = egui::Rect::from_center_size(avail.center(), tex_size * k);
                        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                        draw(ui, rect);
                        (rect, resp)
                    }
                    Scale::Native => {
                        let out = egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                            let (rect, resp) =
                                ui.allocate_exact_size(tex_size, Sense::click_and_drag());
                            draw(ui, rect);
                            (rect, resp)
                        });
                        out.inner
                    }
                };
                if !s.target.view_only {
                    send_input(ctx, s, rect, tex_size, &resp);
                }
            });

        if disconnect {
            self.go_home(ctx, None);
        }
    }
}

fn toolbar(ui: &mut egui::Ui, ctx: &egui::Context, s: &mut Session, disconnect: &mut bool) {
    ui.horizontal(|ui| {
        let dot = if s.target.view_only {
            Color32::from_rgb(0xf5, 0x9e, 0x0b)
        } else {
            Color32::from_rgb(0x22, 0xc5, 0x5e)
        };
        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 4.0, dot);
        ui.label(RichText::new(&s.target.title).strong());
        let (w, h) = s
            .texture
            .as_ref()
            .map(|t| (t.size()[0], t.size()[1]))
            .unwrap_or_default();
        ui.label(RichText::new(format!("{w}×{h}")).color(MUTED).size(12.0));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(RichText::new("Bağlantıyı kes").color(Color32::from_rgb(0xfc, 0xa5, 0xa5)))
                .clicked()
            {
                *disconnect = true;
            }
            let fs = if s.fullscreen {
                "Tam ekrandan çık"
            } else {
                "Tam ekran"
            };
            if ui.button(fs).on_hover_text("Ctrl+Alt+Enter").clicked() {
                toggle_fullscreen(ctx, s);
            }
            let sc = if s.scale == Scale::Fit {
                "Gerçek boyut"
            } else {
                "Pencereye sığdır"
            };
            if ui.button(sc).clicked() {
                s.scale = if s.scale == Scale::Fit {
                    Scale::Native
                } else {
                    Scale::Fit
                };
            }
            if ui
                .add_enabled(!s.target.view_only, egui::Button::new("Ctrl+Alt+Del"))
                .clicked()
            {
                for k in [keys::CONTROL_L, keys::ALT_L, keys::DELETE] {
                    s.sender.key(k, true);
                }
                for k in [keys::DELETE, keys::ALT_L, keys::CONTROL_L] {
                    s.sender.key(k, false);
                }
            }
            ui.toggle_value(&mut s.target.view_only, "Sadece izle");
        });
    });
}

/// Turns common network errors into plain Turkish; falls back to the raw message.
fn explain(e: &anyhow::Error) -> String {
    use std::io::ErrorKind::*;
    let io = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>());
    match io.map(|io| io.kind()) {
        Some(ConnectionRefused) => {
            "Sunucu bağlantıyı reddetti. Adres ve port doğru mu, VNC sunucusu çalışıyor mu?".into()
        }
        Some(TimedOut | WouldBlock) => {
            "Sunucu yanıt vermedi (zaman aşımı). Bilgisayar açık mı, güvenlik duvarı engelliyor olabilir mi?".into()
        }
        Some(UnexpectedEof | ConnectionReset | ConnectionAborted) => "Sunucu bağlantıyı kapattı.".into(),
        Some(HostUnreachable | NetworkUnreachable) => "Bu adrese ağ üzerinden ulaşılamıyor.".into(),
        _ if format!("{e}").starts_with("cannot resolve") => "Adres bulunamadı. Yazımı kontrol edin.".into(),
        _ => format!("{e:#}"),
    }
}

fn toggle_fullscreen(ctx: &egui::Context, s: &mut Session) {
    s.fullscreen = !s.fullscreen;
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(s.fullscreen));
}

fn send_input(
    ctx: &egui::Context,
    s: &mut Session,
    rect: egui::Rect,
    tex: Vec2,
    resp: &egui::Response,
) {
    let to_fb = |p: egui::Pos2| {
        let x = ((p.x - rect.min.x) / rect.width() * tex.x).clamp(0.0, tex.x - 1.0);
        let y = ((p.y - rect.min.y) / rect.height() * tex.y).clamp(0.0, tex.y - 1.0);
        (x as u16, y as u16)
    };
    let typing_locally = ctx.wants_keyboard_input();
    let sender = s.sender.clone();

    ctx.input(|i| {
        // Pointer: while hovering the remote screen, or while dragging that started on it.
        let pos = i
            .pointer
            .latest_pos()
            .filter(|_| resp.contains_pointer() || resp.dragged());
        if let Some(pos) = pos {
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
            if s.scale == Scale::Fit {
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
        }

        if typing_locally {
            return;
        }

        // Modifiers (released when the window loses focus so nothing gets stuck).
        let now = if i.focused {
            i.modifiers
        } else {
            egui::Modifiers::NONE
        };
        for (was, is, keysym) in [
            (s.modifiers.shift, now.shift, keys::SHIFT_L),
            (s.modifiers.ctrl, now.ctrl, keys::CONTROL_L),
            (s.modifiers.alt, now.alt, keys::ALT_L),
            (s.modifiers.mac_cmd, now.mac_cmd, keys::SUPER_L),
        ] {
            if was != is {
                sender.key(keysym, is);
            }
        }
        s.modifiers = now;

        for ev in &i.events {
            match ev {
                egui::Event::Text(text) => {
                    for c in text.chars() {
                        tap(&sender, keys::char_keysym(c));
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
                    if let Some(sym) = keys::special_keysym(*key) {
                        sender.key(sym, *pressed);
                    } else if modifiers.ctrl || modifiers.alt {
                        // Text events are not generated while Ctrl/Alt is held.
                        let name = key.name();
                        if name.chars().count() == 1 {
                            let c = name.chars().next().unwrap().to_ascii_lowercase();
                            sender.key(keys::char_keysym(c), *pressed);
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

fn tap(sender: &Sender, keysym: u32) {
    sender.key(keysym, true);
    sender.key(keysym, false);
}

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .color(Color32::WHITE)
            .strong(),
    )
    .fill(ACCENT)
    .corner_radius(8)
    .min_size(vec2(0.0, 32.0))
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(0x0f, 0x11, 0x15);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.6);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = CornerRadius::same(12);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_gray(55));
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7));
    // Always dark, regardless of the OS theme.
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.style_mut(|s| {
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.5));
        s.text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll(ctx);
        match self.state {
            State::Connected(_) => self.session_ui(ctx),
            _ => {
                self.home_ui(ctx);
                self.connecting_ui(ctx);
            }
        }
        self.editor_ui(ctx);
        self.prompt_ui(ctx);

        // Write changes right away instead of waiting for eframe's periodic autosave.
        if self.store.dirty
            && let Some(storage) = frame.storage_mut()
        {
            eframe::set_value(storage, Store::KEY, &self.store);
            storage.flush();
            self.store.dirty = false;
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, Store::KEY, &self.store);
    }
}
