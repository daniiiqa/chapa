//! chapa — la chapa de discada: bloquea la sesión con ext-session-lock (la
//! misma vía que hyprlock), una superficie por monitor, con el look del rice.
//! Si chapa se cae, Hyprland deja la sesión bloqueada (nunca la abre).
//!
//!   chapa               bloquear
//!   chapa --prueba N    bloquear y abrir sola a los N segundos (para probar
//!                       sin quedarse encerrado)

mod gpu;
mod huella;
mod pam;
mod ui;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop,
            channel::{Event, Sender, channel},
            ping::make_ping,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::time::Duration;
use ui::{Prompt, Status, Ui};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};

struct Surf {
    lock: SessionLockSurface,
    output: wl_output::WlOutput,
    size: (u32, u32),
    /// EGL de esta superficie (con video); None: se dibuja en shm
    target: Option<gpu::Target>,
}

struct App {
    conn: Connection,
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    seats: SeatState,
    shm: Shm,
    pool: Option<SlotPool>,
    gpu: Option<gpu::Gpu>,
    lock_state: SessionLockState,
    lock: Option<SessionLock>,
    locked: bool,
    surfaces: Vec<Surf>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    ui: Ui,
    user: String,
    pass: pam::Secreto,
    status: Status,
    pam_tx: Sender<bool>,
    /// el hilo de la huella; None: sin huella (o ya se abrió)
    huella: Option<huella::Huella>,
    /// el lector está escuchando: la pantalla dice "o huella"
    dedo: bool,
    exit: Option<i32>,
}

fn user() -> String {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() {
            return std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned();
        }
    }
    std::env::var("USER").unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let prueba: Option<u64> = args.iter().position(|a| a == "--prueba").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok());

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chapa: sin Wayland ({e})");
            std::process::exit(1);
        }
    };
    let (globals, mut queue) = registry_queue_init(&conn).expect("registro de Wayland");
    let qh: QueueHandle<App> = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let (pam_tx, pam_rx) = channel::<bool>();

    let mut app = App {
        conn: conn.clone(),
        registry: RegistryState::new(&globals),
        compositor: CompositorState::bind(&globals, &qh).expect("wl_compositor"),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).expect("wl_shm"),
        pool: None,
        gpu: None,
        lock_state: SessionLockState::new(&globals, &qh),
        lock: None,
        locked: false,
        surfaces: Vec::new(),
        keyboard: None,
        ui: Ui::new(),
        user: user(),
        pass: pam::Secreto::new(),
        status: Status::Idle,
        pam_tx,
        huella: None,
        dedo: false,
        exit: None,
    };
    // los monitores primero, para tener una superficie por cada uno
    queue.roundtrip(&mut app).expect("roundtrip");

    // el wallpaper (video o imagen) con mpv sobre EGL; sin eso, fondo sólido
    let (ping, ping_src) = make_ping().expect("ping");
    let source = gpu::wallpaper();
    app.gpu = gpu::Gpu::new(conn.backend().display_ptr() as *mut std::ffi::c_void, &source, ping);
    if app.gpu.is_none() {
        eprintln!("chapa: sin EGL/mpv, fondo sólido");
    }
    event_loop
        .handle()
        .insert_source(ping_src, |_, _, app| {
            if app.gpu.as_ref().is_some_and(|g| g.new_frame()) {
                app.present_all();
            }
        })
        .expect("cuadros de mpv");

    let Ok(lock) = app.lock_state.lock(&qh) else {
        eprintln!("chapa: el compositor no tiene ext-session-lock");
        std::process::exit(1);
    };
    app.lock = Some(lock);
    // la huella, a la par de la contraseña
    let (huella_tx, huella_rx) = channel::<huella::Aviso>();
    app.huella = Some(huella::Huella::empezar(huella_tx));
    event_loop
        .handle()
        .insert_source(huella_rx, |ev, _, app| {
            if let Event::Msg(aviso) = ev {
                match aviso {
                    huella::Aviso::Si => return app.unlock(0),
                    huella::Aviso::Lista => app.dedo = true,
                    huella::Aviso::No => {
                        if app.status != Status::Checking {
                            app.status = Status::Wrong;
                        }
                    }
                    huella::Aviso::Fuera => app.dedo = false,
                }
                app.redraw();
            }
        })
        .expect("canal de la huella");
    for output in app.outputs.outputs() {
        app.add_surface(&qh, output);
    }

    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle()).expect("fuente de Wayland");

    // la respuesta de PAM (desde su hilo)
    event_loop
        .handle()
        .insert_source(pam_rx, |ev, _, app| {
            if let Event::Msg(ok) = ev {
                if ok {
                    app.unlock(0);
                } else {
                    app.status = Status::Wrong;
                    app.redraw();
                }
            }
        })
        .expect("canal de PAM");

    // la hora: redibujar al empezar cada minuto (revisando cada segundo)
    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, app| {
            app.redraw();
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .expect("reloj");

    if let Some(n) = prueba {
        event_loop
            .handle()
            .insert_source(Timer::from_duration(Duration::from_secs(n)), |_, _, app| {
                eprintln!("chapa: --prueba, se abre sola");
                app.unlock(0);
                TimeoutAction::Drop
            })
            .expect("prueba");
    }

    while app.exit.is_none() {
        if event_loop.dispatch(Duration::from_millis(100), &mut app).is_err() {
            // perder Wayland no abre nada: el compositor sigue bloqueado
            if let Some(h) = app.huella.take() {
                h.parar();
            }
            std::process::exit(2);
        }
    }
    // process::exit no corre los Drop: que fprintd suelte el lector
    if let Some(h) = app.huella.take() {
        h.parar();
    }
    std::process::exit(app.exit.unwrap_or(0));
}

impl App {
    fn add_surface(&mut self, qh: &QueueHandle<App>, output: wl_output::WlOutput) {
        let Some(lock) = self.lock.as_ref() else { return };
        let surface = self.compositor.create_surface(qh);
        let lock = lock.create_lock_surface(surface, &output, qh);
        self.surfaces.push(Surf { lock, output, size: (0, 0), target: None });
    }

    fn scale(&self, output: &wl_output::WlOutput) -> i32 {
        self.outputs.info(output).map_or(1, |i| i.scale_factor.max(1))
    }

    fn draw(&mut self, i: usize) {
        let (w, h) = self.surfaces[i].size;
        if w == 0 || h == 0 {
            return;
        }
        let scale = self.scale(&self.surfaces[i].output);
        let (bw, bh) = (w * scale as u32, h * scale as u32);
        let p = Prompt { typed: self.pass.letras(), status: self.status, dedo: self.dedo };
        if let Some(gpu) = self.gpu.as_ref() {
            let surf = &mut self.surfaces[i];
            surf.lock.wl_surface().set_buffer_scale(scale);
            match surf.target.as_mut() {
                Some(t) => gpu.resize(t, bw as i32, bh as i32),
                None => surf.target = gpu.target(surf.lock.wl_surface(), bw as i32, bh as i32),
            }
            if let Some(t) = surf.target.as_mut()
                && let Some(pix) = self.ui.draw(bw, bh, scale as f32, p, true)
            {
                gpu.upload(t, pix.data(), bw as i32, bh as i32);
                gpu.present(t);
                return;
            }
        }
        let Some(pix) = self.ui.draw(bw, bh, scale as f32, p, false) else { return };
        let pool = self.pool.get_or_insert_with(|| SlotPool::new((bw * bh * 4) as usize, &self.shm).expect("shm"));
        let Ok((buffer, canvas)) = pool.create_buffer(bw as i32, bh as i32, bw as i32 * 4, wl_shm::Format::Argb8888) else { return };
        // tiny-skia: RGBA premultiplicado → ARGB8888 (en memoria: B G R A)
        for (d, s) in canvas.chunks_exact_mut(4).zip(pix.data().chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        let surf = self.surfaces[i].lock.wl_surface();
        surf.set_buffer_scale(scale);
        if buffer.attach_to(surf).is_ok() {
            surf.damage_buffer(0, 0, bw as i32, bh as i32);
            surf.commit();
        }
    }

    /// Un cuadro nuevo del wallpaper en cada superficie (la capa de texto no cambia).
    fn present_all(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else { return };
        for s in &self.surfaces {
            if let Some(t) = &s.target {
                gpu.present(t);
            }
        }
    }

    fn redraw(&mut self) {
        for i in 0..self.surfaces.len() {
            self.draw(i);
        }
    }

    fn unlock(&mut self, code: i32) {
        // que fprintd suelte el lector (si se abrió con contraseña)
        if let Some(h) = self.huella.take() {
            h.parar();
        }
        if let Some(lock) = self.lock.take() {
            lock.unlock();
            let _ = self.conn.roundtrip();
        }
        self.exit = Some(code);
    }

    fn submit(&mut self) {
        if self.status == Status::Checking || self.pass.is_empty() {
            return;
        }
        self.status = Status::Checking;
        let (user, pass, tx) = (self.user.clone(), self.pass.take(), self.pam_tx.clone());
        std::thread::spawn(move || {
            // si PAM se cae, es un no (y la chapa no se queda en «…» para siempre)
            let ok = std::panic::catch_unwind(move || pam::check(&user, pass)).unwrap_or(false);
            let _ = tx.send(ok);
        });
        self.redraw();
    }
}

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        self.locked = true;
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        // el compositor no nos dejó bloquear (ya hay otra chapa) o nos quitó el
        // bloqueo. Nunca 0: 0 es «se abrió con la contraseña o la huella», y con
        // cualquier otro LockScreen.sh pone hyprlock por si acaso.
        eprintln!("chapa: el compositor no dejó bloquear o quitó el bloqueo");
        self.exit = Some(if self.locked { 3 } else { 1 });
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: SessionLockSurface, cfg: SessionLockSurfaceConfigure, _: u32) {
        if let Some(i) = self.surfaces.iter().position(|s| s.lock.wl_surface() == surface.wl_surface()) {
            self.surfaces[i].size = cfg.new_size;
            self.draw(i);
        }
    }
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        self.key(ev);
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        self.key(ev);
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

impl App {
    fn key(&mut self, ev: KeyEvent) {
        if self.status == Status::Checking {
            return;
        }
        match ev.keysym {
            Keysym::Return | Keysym::KP_Enter => return self.submit(),
            Keysym::BackSpace => {
                self.pass.pop();
            }
            Keysym::Escape => self.pass.clear(),
            _ => match ev.utf8 {
                Some(t) if !t.chars().any(char::is_control) => self.pass.push_str(&t),
                _ => return,
            },
        }
        if self.status == Status::Wrong {
            self.status = Status::Idle;
        }
        self.redraw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seats.get_keyboard(qh, &seat, None).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {
        self.redraw();
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // un monitor que se conecta con la sesión bloqueada también se tapa
        if self.lock.is_some() && !self.surfaces.iter().any(|s| s.output == output) {
            self.add_surface(qh, output);
        }
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.surfaces.retain(|s| s.output != output);
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
