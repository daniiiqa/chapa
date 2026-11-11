//! `chapa --muestra salida.png`: el cuadro de la chapa en una imagen, sin
//! bloquear nada. Para ver cómo queda cada look (y probar sin encerrarse).
//!
//!   chapa --muestra out.png [--look nombre] [--tam 1920x1200]
//!                           [--letras N] [--mal] [--revisando] [--sin-dedo]
//!
//! El fondo es el wallpaper (la imagen de wallpaper_effects) con el blur del
//! look; con --look, ese look aunque no sea el actual (sus fuentes de su
//! look.toml; los colores, los de ahora).

use std::path::Path;
use std::process::Command;

use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::estilo;
use crate::lienzo::{Estado, Lienzo};
use crate::ui::Status;

fn arg<'a>(args: &'a [String], k: &str) -> Option<&'a str> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// La fuente de un look: `[font] family` y `display` de su look.toml.
fn fuentes(look_toml: &str) -> (String, String) {
    let mut dentro = false;
    let (mut fam, mut disp) = (String::new(), String::new());
    for l in look_toml.lines() {
        let l = l.split('#').next().unwrap_or("").trim();
        if l.starts_with('[') {
            dentro = l == "[font]";
            continue;
        }
        if !dentro {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"').to_string();
            match k.trim() {
                "family" => fam = v,
                "display" => disp = v,
                _ => {}
            }
        }
    }
    if disp.is_empty() {
        disp = fam.clone();
    }
    (fam, disp)
}

/// Un PPM P6 (lo que da `magick … ppm:-`) a pixmap.
fn ppm(datos: &[u8]) -> Option<Pixmap> {
    // cabecera: P6, ancho, alto, máximo, separados por espacios
    let mut campos = Vec::new();
    let mut i = 0;
    while campos.len() < 4 && i < datos.len() {
        while i < datos.len() && datos[i].is_ascii_whitespace() {
            i += 1;
        }
        let ini = i;
        while i < datos.len() && !datos[i].is_ascii_whitespace() {
            i += 1;
        }
        campos.push(std::str::from_utf8(&datos[ini..i]).ok()?.to_string());
    }
    if campos.first()? != "P6" || campos.get(3)? != "255" {
        return None;
    }
    let (w, h): (u32, u32) = (campos[1].parse().ok()?, campos[2].parse().ok()?);
    let rgb = datos.get(i + 1..i + 1 + (w * h * 3) as usize)?;
    let mut pix = Pixmap::new(w, h)?;
    for (d, s) in pix.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0) {
        *d = [s[0], s[1], s[2], 255];
    }
    Some(pix)
}

fn tam_del_monitor() -> (u32, u32) {
    let out = Command::new("hyprctl").args(["monitors", "-j"]).output();
    let j = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let num = |k: &str| j.split(&format!("\"{k}\": ")).nth(1).and_then(|r| r.split([',', '}']).next()).and_then(|v| v.trim().parse().ok());
    (num("width").unwrap_or(1920), num("height").unwrap_or(1200))
}

pub fn correr(args: &[String]) -> i32 {
    let Some(salida) = arg(args, "--muestra") else { return 2 };
    let home = std::env::var("HOME").unwrap_or_default();
    let looks = format!("{home}/.config/hypr/looks");
    let estilo = match arg(args, "--look") {
        Some(look) => {
            let conf = std::fs::read_to_string(format!("{looks}/{look}/hypr/hyprlock.conf"));
            let toml = std::fs::read_to_string(format!("{looks}/{look}/look.toml")).unwrap_or_default();
            let (fam, disp) = fuentes(&toml);
            conf.ok().and_then(|c| {
                let c = c.replace("{{font_display}}", &disp).replace("{{font}}", &fam);
                estilo::cargar(Path::new(""), Some(&c))
            })
        }
        None => estilo::cargar(&estilo::vivo(), None),
    };
    let Some(estilo) = estilo else {
        eprintln!("chapa: no se pudo leer el hyprlock.conf");
        return 1;
    };
    let (w, h) = arg(args, "--tam").and_then(|t| t.split_once('x')).and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).unwrap_or_else(tam_del_monitor);

    // el fondo: el wallpaper como lo pone el look (cubre, centrado, con su blur)
    let wall = format!("{home}/.config/hypr/wallpaper_effects/.wallpaper_current");
    let mut cmd = Command::new("magick");
    cmd.arg(format!("{wall}[0]")).args(["-resize", &format!("{w}x{h}^"), "-gravity", "center", "-extent", &format!("{w}x{h}")]);
    if estilo.fondo.blur > 0.0 {
        cmd.args(["-blur", &format!("0x{}", estilo.fondo.blur * 0.6)]);
    }
    let fondo = cmd.arg("ppm:-").output().ok().and_then(|o| ppm(&o.stdout));

    let estado = Estado {
        letras: arg(args, "--letras").and_then(|n| n.parse().ok()).unwrap_or(0),
        status: if args.iter().any(|a| a == "--mal") {
            Status::Wrong
        } else if args.iter().any(|a| a == "--revisando") {
            Status::Checking
        } else {
            Status::Idle
        },
        dedo: !args.iter().any(|a| a == "--sin-dedo"),
        mayus: false,
        intentos: 1,
    };
    let user = std::env::var("USER").unwrap_or_default();
    let mut lienzo = Lienzo::new(estilo, user);
    let Some(capa) = lienzo.dibujar(w, h, 1.0, &estado, fondo.is_some()) else { return 1 };
    let img = match fondo {
        Some(mut f) => {
            f.draw_pixmap(0, 0, capa.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            f
        }
        None => capa,
    };
    match img.save_png(salida) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("chapa: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ppm_y_fuentes() {
        let p = super::ppm(b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06").unwrap();
        assert_eq!((p.width(), p.height()), (2, 1));
        assert_eq!(&p.data()[..4], &[1, 2, 3, 255]);
        let (f, d) = super::fuentes("[font]\nfamily = \"Mono\"   # x\n[colors]\nfamily = \"no\"\n");
        assert_eq!((f.as_str(), d.as_str()), ("Mono", "Mono"));
    }
}
