//! La pantalla de la chapa, como las otras apps de discada: texto en un
//! `Buffer` de ratatui que disco pinta en pixeles con la fuente de kitty y los
//! colores del look. La hora grande, la fecha, y la contraseña como puntos.

use disco::look::Look;
use disco::paint::Painter;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use tiny_skia::Pixmap;

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    /// esperando la contraseña
    Idle,
    /// PAM está revisando
    Checking,
    /// la última estaba mal
    Wrong,
}

pub struct Ui {
    pub look: Look,
    big: Painter,
    small: Painter,
}

/// strftime con el idioma del sistema.
pub fn now(fmt: &str) -> String {
    let mut out = [0u8; 128];
    let f = std::ffi::CString::new(fmt).unwrap_or_default();
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let n = libc::strftime(out.as_mut_ptr() as *mut libc::c_char, out.len(), f.as_ptr(), &tm);
        String::from_utf8_lossy(&out[..n]).into_owned()
    }
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (12, 12, 14),
    }
}

impl Ui {
    pub fn new() -> Ui {
        unsafe { libc::setlocale(libc::LC_TIME, c"".as_ptr()) };
        Ui { look: Look::load("chapa"), big: Painter::sized(Some(132.0)), small: Painter::sized(Some(17.0)) }
    }

    /// Una línea de texto centrada en x, con su esquina de arriba en `y`.
    fn line(p: &mut Painter, pix: &mut Pixmap, look: &Look, text: &str, style: Style, y: f32, s: f32) {
        let w = text.chars().count().max(1) as u16;
        let mut buf = Buffer::empty(Rect::new(0, 0, w, 1));
        buf.set_string(0, 0, text, style);
        let (pw, _) = p.size(w, 1);
        let x = ((pix.width() as f32 - pw as f32 * s) / 2.0).round();
        // sombra: el mismo texto en negro, corrido, para leerse sobre el video
        let mut dark = buf.clone();
        dark.set_style(dark.area, Style::new().fg(Color::Rgb(0, 0, 0)));
        let d = (p.size(1, 1).1 as f32 * s / 16.0).max(1.5);
        p.cells(pix, &dark, look, x + d, y + d, s);
        p.cells(pix, &buf, look, x, y, s);
    }

    /// Todo el cuadro de un monitor de `w`×`h` pixeles, a escala `s`.
    /// `over`: va encima del wallpaper (velo del color del look, para leer);
    /// si no, fondo sólido.
    pub fn draw(&mut self, w: u32, h: u32, s: f32, typed: usize, status: Status, over: bool) -> Option<Pixmap> {
        let mut pix = Pixmap::new(w, h)?;
        let t = self.look.theme;
        let (r, g, b) = rgb(t.base);
        pix.fill(tiny_skia::Color::from_rgba8(r, g, b, if over { 92 } else { 255 }));

        let hf = h as f32;
        let (_, bh) = self.big.size(1, 1);
        let top = hf * 0.30;
        Ui::line(&mut self.big, &mut pix, &self.look, &now("%H:%M"), Style::new().fg(t.fg).add_modifier(Modifier::BOLD), top, s);
        let date = now("%A %d · %B").to_lowercase();
        Ui::line(&mut self.small, &mut pix, &self.look, &date, Style::new().fg(t.muted), top + bh as f32 * s, s);

        // la contraseña: un punto por letra (máximo 24), o lo que está pasando
        let (text, style) = match status {
            Status::Checking => ("…".to_string(), Style::new().fg(t.primary)),
            Status::Wrong if typed == 0 => ("no · otra vez".to_string(), Style::new().fg(t.red)),
            _ if typed == 0 => ("contraseña".to_string(), Style::new().fg(t.muted)),
            _ => ("● ".repeat(typed.min(24)).trim_end().to_string(), Style::new().fg(t.accent)),
        };
        Ui::line(&mut self.small, &mut pix, &self.look, &text, style, hf * 0.62, s);
        Some(pix)
    }
}
