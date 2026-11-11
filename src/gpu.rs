//! El fondo en video: EGL en cada superficie de la chapa, mpv (libmpv, la
//! misma tripa que mpvpaper) pinta el wallpaper (video o imagen) y encima va
//! la capa de la hora y la contraseña (una textura que solo se sube cuando
//! cambia). Si algo de esto falla, main usa el dibujo en memoria (shm).

use glow::HasContext;
use khronos_egl as egl;
use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;
use wayland_client::Proxy;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_egl::WlEglSurface;

// ─── libmpv ─────────────────────────────────────────────────────────────────

#[repr(C)]
struct Param {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct GlInit {
    get_proc_address: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    ctx: *mut c_void,
}

#[repr(C)]
struct Fbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

const P_INVALID: c_int = 0;
const P_API_TYPE: c_int = 1;
const P_GL_INIT: c_int = 2;
const P_FBO: c_int = 3;
const P_FLIP_Y: c_int = 4;
const P_WL_DISPLAY: c_int = 9;
const UPDATE_FRAME: u64 = 1;

#[link(name = "mpv")]
unsafe extern "C" {
    fn mpv_create() -> *mut c_void;
    fn mpv_initialize(h: *mut c_void) -> c_int;
    fn mpv_set_option_string(h: *mut c_void, name: *const c_char, value: *const c_char) -> c_int;
    fn mpv_command(h: *mut c_void, args: *mut *const c_char) -> c_int;
    fn mpv_render_context_create(out: *mut *mut c_void, h: *mut c_void, params: *mut Param) -> c_int;
    fn mpv_render_context_set_update_callback(ctx: *mut c_void, cb: extern "C" fn(*mut c_void), data: *mut c_void);
    fn mpv_render_context_update(ctx: *mut c_void) -> u64;
    fn mpv_render_context_render(ctx: *mut c_void, params: *mut Param) -> c_int;
    fn mpv_render_context_report_swap(ctx: *mut c_void);
}

static EGL: egl::Instance<egl::Static> = egl::Instance::new(egl::Static);

use std::sync::atomic::{AtomicU8, Ordering};

/// Cómo le fue al último cuadro (0 bien, 1 make_current, 2 swap, 3 mpv). Al log
/// solo cuando cambia: si no, serían 60 líneas por segundo.
static ESTADO: AtomicU8 = AtomicU8::new(0);

fn estado(nuevo: u8, que: impl FnOnce() -> String) {
    if ESTADO.swap(nuevo, Ordering::Relaxed) != nuevo {
        di!("gpu: {}", que());
    }
}

extern "C" fn proc_address(_: *mut c_void, name: *const c_char) -> *mut c_void {
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_str().unwrap_or("");
    EGL.get_proc_address(name).map_or(ptr::null_mut(), |f| f as *mut c_void)
}

/// mpv avisa desde su hilo que hay cuadro nuevo: despertar el loop.
extern "C" fn on_update(data: *mut c_void) {
    let ping = unsafe { &*(data as *const smithay_client_toolkit::reexports::calloop::ping::Ping) };
    ping.ping();
}

// ─── GL ─────────────────────────────────────────────────────────────────────

const VS: &str = "attribute vec2 p; varying vec2 uv;
void main() { uv = vec2(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5); gl_Position = vec4(p, 0.0, 1.0); }";
const FS: &str = "precision mediump float; uniform sampler2D t; varying vec2 uv;
void main() { gl_FragColor = texture2D(t, uv); }";

/// Una superficie de la chapa con su EGL y su capa de texto.
pub struct Target {
    win: WlEglSurface,
    surf: egl::Surface,
    size: (i32, i32),
    tex: Option<glow::Texture>,
    /// con blur: el video se dibuja aquí en chico y se estira (framebuffer, textura, tamaño)
    chico: std::cell::Cell<Option<(glow::Framebuffer, glow::Texture, i32, i32)>>,
}

pub struct Gpu {
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    gl: glow::Context,
    prog: glow::Program,
    vbo: glow::Buffer,
    /// el reproductor (vive lo que vive la chapa)
    _mpv: *mut c_void,
    render: *mut c_void,
    /// el blur del look (blur_size × blur_passes de su hyprlock.conf)
    blur: std::cell::Cell<f32>,
}

impl Gpu {
    /// EGL sobre la conexión de Wayland y mpv con `source` (video o imagen).
    /// `ping` despierta el loop cuando mpv tiene cuadro nuevo.
    pub fn new(wl_display: *mut c_void, source: &str, ping: smithay_client_toolkit::reexports::calloop::ping::Ping) -> Option<Gpu> {
        unsafe {
            let display = EGL.get_display(wl_display as egl::NativeDisplayType)?;
            EGL.initialize(display).ok()?;
            EGL.bind_api(egl::OPENGL_ES_API).ok()?;
            let attrs = [
                egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8, egl::ALPHA_SIZE, 8,
                egl::SURFACE_TYPE, egl::WINDOW_BIT, egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT, egl::NONE,
            ];
            let config = EGL.choose_first_config(display, &attrs).ok()??;
            let context = EGL
                .create_context(display, config, None, &[egl::CONTEXT_MAJOR_VERSION, 3, egl::NONE])
                .or_else(|_| EGL.create_context(display, config, None, &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE]))
                .ok()?;
            // sin superficie todavía: el contexto se puede hacer actual solo
            EGL.make_current(display, None, None, Some(context)).ok()?;
            let gl = glow::Context::from_loader_function(|s| EGL.get_proc_address(s).map_or(ptr::null(), |f| f as *const c_void));

            let prog = gl.create_program().ok()?;
            for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
                let sh = gl.create_shader(kind).ok()?;
                gl.shader_source(sh, src);
                gl.compile_shader(sh);
                if !gl.get_shader_compile_status(sh) {
                    di!("shader: {}", gl.get_shader_info_log(sh));
                    return None;
                }
                gl.attach_shader(prog, sh);
            }
            gl.bind_attrib_location(prog, 0, "p");
            gl.link_program(prog);
            if !gl.get_program_link_status(prog) {
                return None;
            }
            let vbo = gl.create_buffer().ok()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, std::slice::from_raw_parts(quad.as_ptr() as *const u8, 32), glow::STATIC_DRAW);

            // mpv: sin audio, en loop, llenando la pantalla como mpvpaper
            let mpv = mpv_create();
            if mpv.is_null() {
                return None;
            }
            // CHAPA_DEBUG=1 (o el archivo ~/.local/state/chapa/debug, para cuando la
            // lanza hypridle): mpv cuenta todo en ~/.local/state/chapa/mpv.log
            let estado_dir = format!("{}/.local/state/chapa", std::env::var("HOME").unwrap_or_default());
            if std::env::var_os("CHAPA_DEBUG").is_some() || std::path::Path::new(&format!("{estado_dir}/debug")).exists() {
                let log = format!("{estado_dir}/mpv.log");
                for (k, v) in [("log-file", log.as_str()), ("msg-level", "all=v")] {
                    let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
                    mpv_set_option_string(mpv, k.as_ptr(), v.as_ptr());
                }
            }
            for (k, v) in [
                ("vo", "libmpv"), ("hwdec", "auto-safe"), ("audio", "no"), ("loop-file", "inf"), ("keep-open", "yes"),
                ("image-display-duration", "inf"), ("panscan", "1.0"), ("terminal", "no"), ("config", "no"),
                ("load-scripts", "no"), ("osc", "no"), ("input-default-bindings", "no"), ("idle", "yes"),
            ] {
                let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
                mpv_set_option_string(mpv, k.as_ptr(), v.as_ptr());
            }
            if mpv_initialize(mpv) < 0 {
                return None;
            }
            let api = CString::new("opengl").unwrap();
            let mut init = GlInit { get_proc_address: proc_address, ctx: ptr::null_mut() };
            let mut params = [
                Param { kind: P_API_TYPE, data: api.as_ptr() as *mut c_void },
                Param { kind: P_GL_INIT, data: &mut init as *mut GlInit as *mut c_void },
                Param { kind: P_WL_DISPLAY, data: wl_display },
                Param { kind: P_INVALID, data: ptr::null_mut() },
            ];
            let mut render = ptr::null_mut();
            if mpv_render_context_create(&mut render, mpv, params.as_mut_ptr()) < 0 {
                di!("mpv no pudo usar OpenGL");
                return None;
            }
            let ping = Box::into_raw(Box::new(ping));
            mpv_render_context_set_update_callback(render, on_update, ping as *mut c_void);
            let cmd = [CString::new("loadfile").unwrap(), CString::new(source).ok()?];
            let mut argv: Vec<*const c_char> = cmd.iter().map(|c| c.as_ptr()).collect();
            argv.push(ptr::null());
            mpv_command(mpv, argv.as_mut_ptr());
            Some(Gpu { display, config, context, gl, prog, vbo, _mpv: mpv, render, blur: std::cell::Cell::new(0.0) })
        }
    }

    pub fn target(&self, surface: &WlSurface, w: i32, h: i32) -> Option<Target> {
        let win = WlEglSurface::new(surface.id(), w, h).ok()?;
        let surf = unsafe { EGL.create_window_surface(self.display, self.config, win.ptr() as egl::NativeWindowType, None).ok()? };
        Some(Target { win, surf, size: (w, h), tex: None, chico: std::cell::Cell::new(None) })
    }

    pub fn resize(&self, t: &mut Target, w: i32, h: i32) {
        if t.size != (w, h) {
            t.win.resize(w, h, 0, 0);
            t.size = (w, h);
        }
    }

    /// ¿Tiene mpv un cuadro nuevo? (después de que avisó)
    pub fn new_frame(&self) -> bool {
        unsafe { mpv_render_context_update(self.render) & UPDATE_FRAME != 0 }
    }

    /// Sube la capa de texto (RGBA premultiplicado, del tamaño de la superficie).
    pub fn upload(&self, t: &mut Target, rgba: &[u8], w: i32, h: i32) {
        let gl = &self.gl;
        unsafe {
            if EGL.make_current(self.display, Some(t.surf), Some(t.surf), Some(self.context)).is_err() {
                return;
            }
            let tex = match t.tex {
                Some(x) => x,
                None => {
                    let Ok(x) = gl.create_texture() else { return };
                    gl.bind_texture(glow::TEXTURE_2D, Some(x));
                    for p in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                        gl.tex_parameter_i32(glow::TEXTURE_2D, p, glow::LINEAR as i32);
                    }
                    for p in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                        gl.tex_parameter_i32(glow::TEXTURE_2D, p, glow::CLAMP_TO_EDGE as i32);
                    }
                    t.tex = Some(x);
                    x
                }
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_image_2d(
                glow::TEXTURE_2D, 0, glow::RGBA as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(rgba)),
            );
        }
    }

    /// Un cuadro: el wallpaper de mpv y la capa de texto encima.
    pub fn present(&self, t: &Target) {
        let Some(r) = self.dibuja(t) else { return };
        unsafe {
            let swap = EGL.swap_buffers(self.display, t.surf);
            mpv_render_context_report_swap(self.render);
            match (r, swap) {
                (_, Err(e)) => estado(2, || format!("swap falló: {e:?}")),
                (r, _) if r < 0 => estado(3, || format!("mpv no dibujó ({r})")),
                _ => estado(0, || "dibuja bien otra vez".into()),
            }
        }
    }

    /// El cuadro de ahora, en RGB y de arriba abajo (para el telón de farula
    /// al abrir). Se dibuja otra vez sin mostrarlo y se lee de vuelta.
    pub fn captura(&self, t: &Target) -> Option<(u32, u32, Vec<u8>)> {
        self.dibuja(t)?;
        let (w, h) = t.size;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        unsafe {
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            self.gl.read_pixels(0, 0, w, h, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut rgba)));
        }
        // GL lee de abajo arriba
        let fila = (w * 4) as usize;
        let rgb = rgba.chunks_exact(fila).rev().flat_map(|f| f.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]])).collect();
        Some((w as u32, h as u32, rgb))
    }

    /// El blur del look: blur_size × blur_passes de hyprlock (0: nítido).
    pub fn set_blur(&self, blur: f32) {
        self.blur.set(blur.max(0.0));
    }

    /// Cuánto más chico se dibuja el video para desenfocarlo (1: nada).
    fn factor(&self) -> i32 {
        let b = self.blur.get();
        if b <= 0.0 { 1 } else { (1.0 + b / 3.0).round().clamp(2.0, 12.0) as i32 }
    }

    /// El framebuffer chico de `t` para el blur (lo crea o lo rehace si cambió
    /// el tamaño). None: sin blur, o la GPU no pudo.
    fn chico(&self, t: &Target) -> Option<(glow::Framebuffer, glow::Texture, i32, i32)> {
        let k = self.factor();
        if k <= 1 {
            return None;
        }
        let (cw, ch) = ((t.size.0 / k).max(1), (t.size.1 / k).max(1));
        if let Some(c) = t.chico.get()
            && (c.2, c.3) == (cw, ch)
        {
            return Some(c);
        }
        let gl = &self.gl;
        unsafe {
            if let Some((fb, tex, _, _)) = t.chico.take() {
                gl.delete_framebuffer(fb);
                gl.delete_texture(tex);
            }
            let tex = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            for p in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, glow::LINEAR as i32);
            }
            for p in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, glow::CLAMP_TO_EDGE as i32);
            }
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32, cw, ch, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            let fb = gl.create_framebuffer().ok()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
            let ok = gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if !ok {
                di!("gpu: el framebuffer del blur no quedó; sin blur");
                gl.delete_framebuffer(fb);
                gl.delete_texture(tex);
                self.blur.set(0.0);
                return None;
            }
            di!("gpu: blur ×{k} ({cw}x{ch})");
            t.chico.set(Some((fb, tex, cw, ch)));
            Some((fb, tex, cw, ch))
        }
    }

    /// mpv y la capa de texto en el framebuffer de `t`, sin mostrarlo. None si
    /// no se pudo usar su superficie; si no, lo que dijo mpv.
    fn dibuja(&self, t: &Target) -> Option<c_int> {
        let gl = &self.gl;
        let (w, h) = t.size;
        unsafe {
            if let Err(e) = EGL.make_current(self.display, Some(t.surf), Some(t.surf), Some(self.context)) {
                estado(1, || format!("make_current falló: {e:?}"));
                return None;
            }
            let _ = EGL.swap_interval(self.display, 0);
            // con blur, mpv dibuja en chico (en una textura) y luego se estira
            // con filtro lineal: un desenfoque suave casi gratis
            let chico = self.chico(t);
            let (destino, dw, dh) = match chico {
                Some((fb, _, cw, ch)) => (fb.0.get() as c_int, cw, ch),
                None => (0, w, h),
            };
            let mut fbo = Fbo { fbo: destino, w: dw, h: dh, internal_format: 0 };
            // con flip, la fila de arriba de la imagen queda en la fila 0: así la
            // muestra bien el framebuffer de la pantalla y también el shader
            let mut flip: c_int = 1;
            let mut params = [
                Param { kind: P_FBO, data: &mut fbo as *mut Fbo as *mut c_void },
                Param { kind: P_FLIP_Y, data: &mut flip as *mut c_int as *mut c_void },
                Param { kind: P_INVALID, data: ptr::null_mut() },
            ];
            let r = mpv_render_context_render(self.render, params.as_mut_ptr());
            if let Some((_, tex, _, _)) = chico {
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.viewport(0, 0, w, h);
                gl.use_program(Some(self.prog));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 0, 0);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.disable(glow::BLEND);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            // mpv deja el estado de GL a su gusto: ponerlo todo de nuevo
            if let Some(tex) = t.tex {
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.viewport(0, 0, w, h);
                gl.use_program(Some(self.prog));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 0, 0);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                gl.disable(glow::BLEND);
            }
            Some(r)
        }
    }
}

/// Lo que va de fondo: el video de mpvpaper (livewallpaper.conf) si hay; si
/// no, la imagen del wallpaper (la que deja WallustSwww).
pub fn wallpaper() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let live = std::fs::read_to_string(format!("{home}/.config/hypr/conf/livewallpaper.conf")).unwrap_or_default();
    let video = live.lines().filter(|l| l.trim_start().starts_with("exec-once") && l.contains("mpvpaper")).find_map(|l| {
        let q: Vec<&str> = l.split('"').collect();
        q.iter().rev().find(|s| {
            let s = s.to_lowercase();
            [".mp4", ".webm", ".mkv", ".mov", ".m4v", ".avi"].iter().any(|e| s.ends_with(e))
        }).map(|s| s.replace("$HOME", &home))
    });
    match video {
        Some(v) if std::path::Path::new(&v).exists() => v,
        _ => format!("{home}/.config/hypr/wallpaper_effects/.wallpaper_current"),
    }
}
