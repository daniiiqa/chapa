//! Dibuja el estilo del look (lo de estilo.rs) como lo dibujaría hyprlock:
//! etiquetas con cualquier fuente (cosmic-text), formas, la entrada con sus
//! puntos y colores de estado, y el velo del fondo.
//!
//! Coordenadas como hyprlock: `position` en pixeles por la escala del
//! monitor, la `y` hacia arriba, medida según halign/valign.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmic_text::{Attrs, Buffer, Color as CColor, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};
use tiny_skia::{FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Rect, SpreadMode, Transform};

use crate::estilo::{Alinea, Color, Cosa, Entrada, Estilo, Etiqueta, Forma, Lugar, Pintura, Texto};
use crate::ui::Status;

/// Lo que la entrada tiene que saber para dibujarse.
#[derive(Clone, Copy)]
pub struct Estado {
    pub letras: usize,
    pub status: Status,
    /// el lector de huellas escucha
    pub dedo: bool,
    pub mayus: bool,
    pub intentos: u32,
}

pub struct Lienzo {
    pub estilo: Estilo,
    fonts: FontSystem,
    cache: SwashCache,
    /// la última salida de cada `cmd[…]`, por índice de `estilo.cosas`
    salidas: Arc<Mutex<Vec<Option<String>>>>,
    user: String,
    layout: String,
}

/// Lo que una etiqueta dice: un `cmd` se corre con sh, como en hyprlock.
fn correr(cmd: &str) -> String {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
        .unwrap_or_default()
}

/// La distribución del teclado, corta ("English (US)" → "en"), para $LAYOUT.
fn layout() -> String {
    let out = std::process::Command::new("hyprctl").args(["devices", "-j"]).stderr(std::process::Stdio::null()).output();
    let json = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    // sin serde: el "active_keymap" del teclado principal (el que trae "main": true)
    let teclados = json.split("\"keyboards\"").nth(1).unwrap_or("");
    teclados
        .split('{')
        .filter(|k| k.contains("\"main\": true"))
        .find_map(|k| k.split("\"active_keymap\": \"").nth(1)?.split('"').next().map(String::from))
        .map(|k| k.chars().filter(|c| c.is_alphabetic()).take(2).collect::<String>().to_lowercase())
        .unwrap_or_default()
}

impl Lienzo {
    pub fn new(estilo: Estilo, user: String) -> Lienzo {
        // la primera vez, ya: que el primer cuadro salga completo
        let salidas: Vec<Option<String>> = estilo
            .cosas
            .iter()
            .map(|c| match c {
                Cosa::Texto(Etiqueta { texto: Texto::Cmd { cmd, .. }, .. }) => Some(correr(cmd)),
                _ => None,
            })
            .collect();
        let salidas = Arc::new(Mutex::new(salidas));
        // luego, cada quien a su ritmo, en un hilo (sh no traba el dibujo)
        let cmds: Vec<(usize, String, Duration)> = estilo
            .cosas
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                Cosa::Texto(Etiqueta { texto: Texto::Cmd { cmd, cada }, .. }) => Some((i, cmd.clone(), *cada)),
                _ => None,
            })
            .collect();
        if !cmds.is_empty() {
            let s = salidas.clone();
            std::thread::spawn(move || {
                let mut toca: Vec<Instant> = cmds.iter().map(|(_, _, c)| Instant::now() + *c).collect();
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    for (k, (i, cmd, cada)) in cmds.iter().enumerate() {
                        if Instant::now() >= toca[k] {
                            let out = correr(cmd);
                            if let Ok(mut v) = s.lock() {
                                v[*i] = Some(out);
                            }
                            toca[k] = Instant::now() + *cada;
                        }
                    }
                }
            });
        }
        Lienzo { estilo, fonts: FontSystem::new(), cache: SwashCache::new(), salidas, user, layout: layout() }
    }

    /// Las variables de hyprlock que quedan en el texto.
    fn variables(&self, t: &str, e: &Estado) -> String {
        let es = std::env::var("LANG").is_ok_and(|l| l.starts_with("es"));
        let fail = if es { "contraseña incorrecta" } else { "wrong password" };
        t.replace("$USER", &self.user)
            .replace("$LAYOUT", &self.layout)
            .replace("$TIME12", &crate::ui::now("%I:%M %p"))
            .replace("$TIME", &crate::ui::now("%H:%M"))
            .replace("$FAIL", fail)
            .replace("$ATTEMPTS", &e.intentos.to_string())
            .replace("$FPRINTPROMPT", "")
            .replace("$FPRINTFAIL", "")
    }

    /// Todo el cuadro de un monitor de `w`×`h` pixeles a escala `s`. `encima`:
    /// va sobre el video (solo el velo del brillo); si no, fondo sólido.
    pub fn dibujar(&mut self, w: u32, h: u32, s: f32, e: &Estado, encima: bool) -> Option<Pixmap> {
        let mut pix = Pixmap::new(w, h)?;
        let f = &self.estilo.fondo;
        if encima {
            // hyprlock multiplica la imagen por el brillo: un velo negro hace lo mismo
            let velo = (1.0 - f.brillo).clamp(0.0, 1.0);
            pix.fill(tiny_skia::Color::from_rgba(0.0, 0.0, 0.0, velo).unwrap_or(tiny_skia::Color::TRANSPARENT));
        } else {
            let [r, g, b, _] = f.color;
            pix.fill(tiny_skia::Color::from_rgba(r * f.brillo.min(1.0), g * f.brillo.min(1.0), b * f.brillo.min(1.0), 1.0).unwrap_or(tiny_skia::Color::BLACK));
        }
        let salidas = self.salidas.lock().map(|v| v.clone()).unwrap_or_default();
        for i in 0..self.estilo.cosas.len() {
            match self.estilo.cosas[i].clone() {
                Cosa::Texto(t) => {
                    let crudo = match &t.texto {
                        Texto::Fijo(x) => x.clone(),
                        Texto::Cmd { .. } => salidas.get(i).cloned().flatten().unwrap_or_default(),
                    };
                    let texto = self.variables(&crudo, e);
                    self.etiqueta(&mut pix, &t, &texto, s);
                }
                Cosa::Forma(fm) => forma(&mut pix, &fm, s),
                Cosa::Entrada(en) => self.entrada(&mut pix, &en, e, s),
            }
        }
        Some(pix)
    }

    fn etiqueta(&mut self, pix: &mut Pixmap, t: &Etiqueta, texto: &str, s: f32) {
        if texto.trim().is_empty() {
            return;
        }
        let Some(capa) = self.texto(texto, &t.familia, t.tam * s, t.color) else { return };
        let (x, y) = colocar(pix, t.lugar, capa.width() as f32, capa.height() as f32, s);
        if let Some(sh) = &t.sombra {
            let sombra = sombra(&capa, sh.color, (sh.tam * s).max(1.0) as usize, sh.pasadas.max(1));
            let m = (sombra.width() - capa.width()) as f32 / 2.0;
            pix.draw_pixmap((x - m) as i32, (y - m) as i32, sombra.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
        }
        pix.draw_pixmap(x as i32, y as i32, capa.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
    }

    /// Texto con marcado de pango (<b>, <i>, <span foreground>) en una capa
    /// del tamaño justo.
    fn texto(&mut self, marcado: &str, familia: &str, px: f32, color: Color) -> Option<Pixmap> {
        let (fam, peso) = partir_familia(familia);
        let trozos = trozos(marcado);
        let base = Attrs::new().family(Family::Name(&fam)).weight(Weight(peso)).color(ccolor(color));
        let spans: Vec<(&str, Attrs)> = trozos
            .iter()
            .map(|t| {
                let mut a = base.clone();
                if t.negrita {
                    a = a.weight(Weight(peso.max(700)));
                }
                if t.cursiva {
                    a = a.style(Style::Italic);
                }
                if let Some(c) = t.color {
                    a = a.color(ccolor(c));
                }
                (t.texto.as_str(), a)
            })
            .collect();
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(px, (px * 1.2).ceil()));
        buf.set_size(None, None);
        buf.set_rich_text(spans, &base, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.fonts, false);
        let (mut ancho, mut alto) = (0.0f32, 0.0f32);
        for run in buf.layout_runs() {
            ancho = ancho.max(run.line_w);
            alto = alto.max(run.line_top + run.line_height);
        }
        let (cw, ch) = (ancho.ceil().max(1.0) as u32, alto.ceil().max(1.0) as u32);
        let mut capa = Pixmap::new(cw, ch)?;
        let datos = capa.data_mut();
        buf.draw(&mut self.fonts, &mut self.cache, ccolor(color), |x, y, w, h, c| {
            let [r, g, b, a] = c.as_rgba();
            if a == 0 {
                return;
            }
            for yy in y.max(0)..(y + h as i32).min(ch as i32) {
                for xx in x.max(0)..(x + w as i32).min(cw as i32) {
                    mezclar(datos, ((yy as u32 * cw + xx as u32) * 4) as usize, [r, g, b, a]);
                }
            }
        });
        Some(capa)
    }

    fn entrada(&mut self, pix: &mut Pixmap, en: &Entrada, e: &Estado, s: f32) {
        let vacia = e.letras == 0;
        // fade_on_empty: vacía y sin nada que avisar, no se ve (como hyprlock)
        if en.ocultar_vacio && vacia && e.status == Status::Idle && !e.dedo {
            return;
        }
        let (w, h) = (en.w * s, en.h * s);
        let (x, y) = colocar(pix, en.lugar, w, h, s);
        let r = if en.redondeo < 0.0 { h / 2.0 } else { en.redondeo * s };
        let borde_pintura = match e.status {
            Status::Checking => &en.check,
            Status::Wrong => &en.fail,
            _ if e.mayus => en.capslock.as_ref().unwrap_or(&en.exterior),
            _ => &en.exterior,
        };
        let b = en.borde * s;
        rect(pix, x + b, y + b, w - 2.0 * b, h - 2.0 * b, (r - b).max(0.0), &Pintura::Solido(en.interior));
        if b > 0.0 {
            anillo(pix, x, y, w, h, r, b, borde_pintura);
        }

        // adentro: los puntos, o lo que toca decir
        let (ix, iw) = (x + b, w - 2.0 * b);
        if !vacia && !en.ocultar_letras {
            let d = (en.puntos * h).max(2.0);
            let paso = d + en.espacio * d;
            let cabe = ((iw - h * 0.5) / paso).floor().max(1.0) as usize;
            let n = e.letras.min(cabe);
            let total = n as f32 * paso - en.espacio * d;
            let x0 = if en.centrados { ix + (iw - total) / 2.0 } else { ix + h * 0.35 };
            let cy = y + h / 2.0;
            for k in 0..n {
                circulo(pix, x0 + k as f32 * paso + d / 2.0, cy, d / 2.0, en.letra);
            }
            return;
        }
        let texto = match e.status {
            Status::Checking => "…".to_string(),
            Status::Wrong => self.variables(&en.fail_text, e),
            _ if e.dedo => format!("{}  󰈷", self.variables(&en.placeholder, e)),
            _ => self.variables(&en.placeholder, e),
        };
        if texto.trim().is_empty() {
            return;
        }
        let Some(capa) = self.texto(&texto, &en.familia, (h * 0.4).max(8.0), en.letra) else { return };
        // centrado, o a la izquierda si los puntos no van al centro (como hyprlock)
        let tx = if en.centrados { ix + (iw - capa.width() as f32) / 2.0 } else { ix + h * 0.35 };
        let ty = y + (h - capa.height() as f32) / 2.0;
        pix.draw_pixmap(tx as i32, ty as i32, capa.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
    }
}

/// La esquina de arriba a la izquierda de algo de `w`×`h`, como hyprlock.
fn colocar(pix: &Pixmap, l: Lugar, w: f32, h: f32, s: f32) -> (f32, f32) {
    let (pw, ph) = (pix.width() as f32, pix.height() as f32);
    let (px, py) = (l.x * s, l.y * s);
    let x = match l.h {
        Alinea::Inicio => px,
        Alinea::Centro => (pw - w) / 2.0 + px,
        Alinea::Fin => pw - w + px,
    };
    // la y de hyprlock sube; desde abajo, al centro o desde arriba
    let abajo = match l.v {
        Alinea::Fin => py,
        Alinea::Centro => (ph - h) / 2.0 + py,
        Alinea::Inicio => ph - h + py,
    };
    (x.round(), (ph - abajo - h).round())
}

fn ccolor(c: Color) -> CColor {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    CColor::rgba(b(c[0]), b(c[1]), b(c[2]), b(c[3]))
}

/// RGBA sin premultiplicar sobre el pixmap (premultiplicado).
fn mezclar(datos: &mut [u8], i: usize, [r, g, b, a]: [u8; 4]) {
    let a = a as u32;
    let inv = 255 - a;
    datos[i] = ((r as u32 * a + datos[i] as u32 * inv) / 255) as u8;
    datos[i + 1] = ((g as u32 * a + datos[i + 1] as u32 * inv) / 255) as u8;
    datos[i + 2] = ((b as u32 * a + datos[i + 2] as u32 * inv) / 255) as u8;
    datos[i + 3] = (a + datos[i + 3] as u32 * inv / 255) as u8;
}

/// "MonaspiceNe Nerd Font Light" → ("MonaspiceNe Nerd Font", 300).
pub fn partir_familia(s: &str) -> (String, u16) {
    const PESOS: [(&str, u16); 12] = [
        ("thin", 100), ("hairline", 100), ("extralight", 200), ("ultralight", 200), ("light", 300), ("regular", 400),
        ("medium", 500), ("semibold", 600), ("demibold", 600), ("bold", 700), ("extrabold", 800), ("black", 900),
    ];
    let palabras: Vec<&str> = s.split_whitespace().collect();
    if let Some((ult, resto)) = palabras.split_last() {
        let l = ult.to_lowercase();
        if let Some((_, p)) = PESOS.iter().find(|(n, _)| *n == l)
            && !resto.is_empty()
        {
            return (resto.join(" "), *p);
        }
    }
    (s.trim().to_string(), 400)
}

#[derive(Debug, Clone, PartialEq)]
struct Trozo {
    texto: String,
    negrita: bool,
    cursiva: bool,
    color: Option<Color>,
}

/// Lo poco de pango que usan los looks: <b>, <i>, <span foreground="#…">.
/// Las otras etiquetas se ignoran; &amp; y compañía se ponen.
fn trozos(s: &str) -> Vec<Trozo> {
    let mut out = Vec::new();
    let (mut negrita, mut cursiva) = (0u32, 0u32);
    let mut colores: Vec<Option<Color>> = Vec::new();
    let mut resto = s;
    let mut actual = String::new();
    let empujar = |t: &mut String, negrita: u32, cursiva: u32, color: Option<Color>, out: &mut Vec<Trozo>| {
        if !t.is_empty() {
            let texto = t.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&");
            out.push(Trozo { texto, negrita: negrita > 0, cursiva: cursiva > 0, color });
            t.clear();
        }
    };
    while let Some(i) = resto.find('<') {
        actual.push_str(&resto[..i]);
        let Some(j) = resto[i..].find('>') else {
            actual.push_str(&resto[i..]);
            resto = "";
            break;
        };
        let tag = &resto[i + 1..i + j];
        let color_ahora = colores.iter().rev().find_map(|c| *c);
        empujar(&mut actual, negrita, cursiva, color_ahora, &mut out);
        let nombre = tag.split_whitespace().next().unwrap_or("").to_lowercase();
        match nombre.as_str() {
            "b" => negrita += 1,
            "/b" => negrita = negrita.saturating_sub(1),
            "i" => cursiva += 1,
            "/i" => cursiva = cursiva.saturating_sub(1),
            "span" => {
                let c = tag
                    .split("foreground=")
                    .nth(1)
                    .map(|v| v.trim_start_matches(['"', '\'']).split(['"', '\'']).next().unwrap_or(""))
                    .and_then(hex_color);
                colores.push(c);
            }
            "/span" => {
                colores.pop();
            }
            _ => {}
        }
        resto = &resto[i + j + 1..];
    }
    actual.push_str(resto);
    let color_ahora = colores.iter().rev().find_map(|c| *c);
    empujar(&mut actual, negrita, cursiva, color_ahora, &mut out);
    out
}

/// "#rrggbb" o "#rrggbbaa".
fn hex_color(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    let v = u32::from_str_radix(h, 16).ok()?;
    let c = |sh: u32| ((v >> sh) & 0xff) as f32 / 255.0;
    match h.len() {
        6 => Some([c(16), c(8), c(0), 1.0]),
        8 => Some([c(24), c(16), c(8), c(0)]),
        _ => None,
    }
}

fn paint_de(p: &Pintura, x: f32, y: f32, w: f32, h: f32) -> Paint<'static> {
    let tc = |c: Color| tiny_skia::Color::from_rgba(c[0], c[1], c[2], c[3]).unwrap_or(tiny_skia::Color::BLACK);
    let mut paint = Paint { anti_alias: true, ..Paint::default() };
    match p {
        Pintura::Solido(c) => paint.set_color(tc(*c)),
        Pintura::Degradado(cs, ang) => {
            // como hyprlock: 0° de izquierda a derecha, girando contra el reloj
            let (sn, cs_) = (ang.to_radians().sin(), ang.to_radians().cos());
            let (cx, cy, r) = (x + w / 2.0, y + h / 2.0, (w.max(h)) / 2.0);
            let a = Point::from_xy(cx - cs_ * r, cy + sn * r);
            let b = Point::from_xy(cx + cs_ * r, cy - sn * r);
            let n = (cs.len() - 1).max(1) as f32;
            let stops: Vec<GradientStop> = cs.iter().enumerate().map(|(i, c)| GradientStop::new(i as f32 / n, tc(*c))).collect();
            match LinearGradient::new(a, b, stops, SpreadMode::Pad, Transform::identity()) {
                Some(sh) => paint.shader = sh,
                None => paint.set_color(tc(cs[0])),
            }
        }
    }
    paint
}

/// Un rectángulo redondeado relleno.
fn rect(pix: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, p: &Pintura) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let Some(path) = redondeado(x, y, w, h, r) else { return };
    let paint = paint_de(p, x, y, w, h);
    pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn redondeado(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if r <= 0.0 {
        return Some(PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?));
    }
    // curvas de cuarto de círculo (k: el punto de control de una bézier cúbica)
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn circulo(pix: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Color) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
        let paint = paint_de(&Pintura::Solido(c), 0.0, 0.0, 0.0, 0.0);
        pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn forma(pix: &mut Pixmap, f: &Forma, s: f32) {
    let (w, h) = (f.w * s, f.h * s);
    let (x, y) = colocar(pix, f.lugar, w, h, s);
    let r = if f.redondeo < 0.0 { w.min(h) / 2.0 } else { f.redondeo * s };
    let b = f.borde * s;
    rect(pix, x + b, y + b, w - 2.0 * b, h - 2.0 * b, (r - b).max(0.0), &Pintura::Solido(f.color));
    if b > 0.0 {
        anillo(pix, x, y, w, h, r, b, &f.borde_color);
    }
}

/// Solo el borde, de grueso `b`: el hueco de adentro queda como estaba (si el
/// relleno es transparente, se ve lo de atrás, no el color del borde).
#[allow(clippy::too_many_arguments)]
fn anillo(pix: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, b: f32, p: &Pintura) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let (Some(fuera), Some(dentro)) = (redondeado(x, y, w, h, r), redondeado(x + b, y + b, w - 2.0 * b, h - 2.0 * b, (r - b).max(0.0))) else {
        return rect(pix, x, y, w, h, r, p);
    };
    let mut pb = PathBuilder::new();
    pb.push_path(&fuera);
    pb.push_path(&dentro);
    let Some(path) = pb.finish() else { return };
    let paint = paint_de(p, x, y, w, h);
    pix.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
}

/// La sombra de una capa de texto: su silueta en `color`, desenfocada
/// (`pasadas` cajas de radio `radio`), con margen para que quepa.
fn sombra(capa: &Pixmap, color: Color, radio: usize, pasadas: u32) -> Pixmap {
    let m = radio * pasadas as usize;
    let (w, h) = (capa.width() as usize + 2 * m, capa.height() as usize + 2 * m);
    let mut alfa = vec![0f32; w * h];
    for (i, px) in capa.pixels().iter().enumerate() {
        let (x, y) = (i % capa.width() as usize, i / capa.width() as usize);
        alfa[(y + m) * w + x + m] = px.alpha() as f32 / 255.0;
    }
    for _ in 0..pasadas {
        caja(&mut alfa, w, h, radio, true);
        caja(&mut alfa, w, h, radio, false);
    }
    let mut out = Pixmap::new(w as u32, h as u32).expect("tamaño");
    let fuerza = color[3];
    for (i, px) in out.data_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
        // hyprlock la refuerza un poco: que se note detrás del texto
        let a = (alfa[i] * 1.6).min(1.0) * fuerza;
        *px = [(color[0] * a * 255.0) as u8, (color[1] * a * 255.0) as u8, (color[2] * a * 255.0) as u8, (a * 255.0) as u8];
    }
    out
}

/// Un desenfoque de caja en una dirección (corre un promedio).
fn caja(a: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    if r == 0 {
        return;
    }
    let (n, lineas) = if horizontal { (w, h) } else { (h, w) };
    let idx = |linea: usize, k: usize| if horizontal { linea * w + k } else { k * w + linea };
    let mut fila = vec![0f32; n];
    for linea in 0..lineas {
        for (k, f) in fila.iter_mut().enumerate() {
            *f = a[idx(linea, k)];
        }
        let mut suma: f32 = fila[..r.min(n)].iter().sum();
        for k in 0..n {
            if k + r < n {
                suma += fila[k + r];
            }
            if k > r {
                suma -= fila[k - r - 1];
            }
            a[idx(linea, k)] = suma / (2 * r + 1) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estilo::{Alinea::*, Lugar};

    #[test]
    fn familias_y_pesos() {
        assert_eq!(partir_familia("MonaspiceNe Nerd Font Light"), ("MonaspiceNe Nerd Font".into(), 300));
        assert_eq!(partir_familia("Noto Serif"), ("Noto Serif".into(), 400));
        assert_eq!(partir_familia("Inter ExtraBold"), ("Inter".into(), 800));
        assert_eq!(partir_familia("Bold"), ("Bold".into(), 400), "solo el peso no es familia");
    }

    #[test]
    fn marcado_de_pango() {
        let t = trozos("<span foreground=\"#6f6d6a\">contraseña &amp; ─</span> <b>ya</b>");
        assert_eq!(t[0].texto, "contraseña & ─");
        assert!(t[0].color.is_some());
        assert_eq!(t[1].texto, " ");
        assert!(t[2].negrita && t[2].texto == "ya");
        assert_eq!(trozos("sin nada")[0].texto, "sin nada");
    }

    #[test]
    fn se_coloca_como_hyprlock() {
        let pix = Pixmap::new(1000, 800).unwrap();
        // abajo a la izquierda, 80 a la derecha y 50 arriba del borde
        assert_eq!(colocar(&pix, Lugar { x: 80.0, y: 50.0, h: Inicio, v: Fin }, 100.0, 40.0, 1.0), (80.0, 710.0));
        // centro, 120 arriba del centro
        assert_eq!(colocar(&pix, Lugar { x: 0.0, y: 120.0, h: Centro, v: Centro }, 100.0, 40.0, 1.0), (450.0, 260.0));
        // arriba a la derecha, -50 y -40 (hacia adentro)
        assert_eq!(colocar(&pix, Lugar { x: -50.0, y: -40.0, h: Fin, v: Inicio }, 100.0, 40.0, 1.0), (850.0, 40.0));
        // la escala lo mueve todo
        assert_eq!(colocar(&pix, Lugar { x: 80.0, y: 50.0, h: Inicio, v: Fin }, 100.0, 40.0, 2.0), (160.0, 660.0));
    }
}
