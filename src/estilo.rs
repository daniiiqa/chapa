//! El estilo de cada look: chapa lee el mismo hyprlock.conf que el look ya
//! trae (el vivo, ~/.config/hypr/hyprlock.conf, que leña llena), así cada look
//! tiene un solo diseño y hyprlock, si entra de repuesto, se ve igual.
//!
//! Lo que entiende de hyprlang: `source =`, `$variables`, bloques, `#`
//! comentarios (`##` es un # de verdad) y los colores rgb()/rgba()/0xAARRGGBB
//! con degradados ("$a $b 45deg"). De hyprlock: background, label, shape e
//! input-field con lo que usan los looks de discada.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// RGBA de 0 a 1.
pub type Color = [f32; 4];

#[derive(Debug, Clone, PartialEq)]
pub enum Pintura {
    Solido(Color),
    /// colores y ángulo en grados (como hyprlock: 0 = de izquierda a derecha)
    Degradado(Vec<Color>, f32),
}

impl Pintura {
    pub fn primero(&self) -> Color {
        match self {
            Pintura::Solido(c) => *c,
            Pintura::Degradado(v, _) => v.first().copied().unwrap_or([0.0; 4]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alinea {
    Inicio,
    Centro,
    Fin,
}

/// Dónde va algo: como hyprlock, la `y` crece hacia arriba y se mide desde
/// abajo, el centro o arriba según `valign`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lugar {
    pub x: f32,
    pub y: f32,
    pub h: Alinea,
    pub v: Alinea,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Texto {
    Fijo(String),
    /// `cmd[update:N] …`: se corre con sh cada N ms
    Cmd { cmd: String, cada: Duration },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sombra {
    pub tam: f32,
    pub pasadas: u32,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Etiqueta {
    pub texto: Texto,
    pub color: Color,
    pub familia: String,
    pub tam: f32,
    pub lugar: Lugar,
    pub sombra: Option<Sombra>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Forma {
    pub w: f32,
    pub h: f32,
    pub color: Color,
    pub redondeo: f32,
    pub borde: f32,
    pub borde_color: Pintura,
    pub lugar: Lugar,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entrada {
    pub w: f32,
    pub h: f32,
    pub borde: f32,
    pub exterior: Pintura,
    pub interior: Color,
    pub letra: Color,
    /// tamaño de los puntos, como fracción del alto
    pub puntos: f32,
    /// espacio entre puntos, como fracción del punto
    pub espacio: f32,
    pub centrados: bool,
    /// -1: redondo del todo
    pub redondeo: f32,
    pub check: Pintura,
    pub fail: Pintura,
    pub capslock: Option<Pintura>,
    pub placeholder: String,
    pub fail_text: String,
    pub ocultar_vacio: bool,
    pub ocultar_letras: bool,
    pub familia: String,
    pub lugar: Lugar,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cosa {
    Texto(Etiqueta),
    Forma(Forma),
    Entrada(Entrada),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fondo {
    pub color: Color,
    /// 1 = como está; hyprlock multiplica la imagen por esto
    pub brillo: f32,
    /// blur_size × blur_passes (0: sin desenfoque)
    pub blur: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Estilo {
    pub fondo: Fondo,
    /// en orden de zindex (lo de abajo primero)
    pub cosas: Vec<Cosa>,
}

/// Un bloque leído: su nombre y sus claves (ya con las variables puestas).
struct Bloque {
    nombre: String,
    claves: HashMap<String, String>,
}

/// Quita el comentario: `#` lo empieza, `##` es un # de verdad.
fn sin_comentario(l: &str) -> String {
    let mut out = String::new();
    let mut it = l.chars().peekable();
    while let Some(c) = it.next() {
        if c == '#' {
            if it.peek() == Some(&'#') {
                it.next();
                out.push('#');
                continue;
            }
            break;
        }
        out.push(c);
    }
    out
}

/// Pone las `$variables` conocidas (la más larga primero: $bg0_a00 antes que
/// $bg0). Las de hyprlock ($USER, $LAYOUT, $FAIL…) se quedan para después.
fn poner(v: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = v;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let largo = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(tail.len());
        let nombre = &tail[..largo];
        // el nombre entero o nada: un "$bg0_acc" sin definir se queda como está
        // (y ese color cae al de antemano), no se vuelve "$bg0" + "_acc"
        match vars.get(nombre) {
            Some(val) if !nombre.is_empty() => out.push_str(val),
            _ => {
                out.push('$');
                out.push_str(nombre);
            }
        }
        rest = &tail[largo..];
    }
    out.push_str(rest);
    out
}

fn leer(path: &Path, vars: &mut HashMap<String, String>, bloques: &mut Vec<Bloque>, hondo: u8) {
    if let Ok(texto) = std::fs::read_to_string(path) {
        leer_texto(&texto, vars, bloques, hondo);
    }
}

fn leer_texto(texto: &str, vars: &mut HashMap<String, String>, bloques: &mut Vec<Bloque>, hondo: u8) {
    let mut actual: Option<Bloque> = None;
    for crudo in texto.lines() {
        let l = sin_comentario(crudo);
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        if l == "}" {
            if let Some(b) = actual.take() {
                bloques.push(b);
            }
            continue;
        }
        if let Some(n) = l.strip_suffix('{') {
            actual = Some(Bloque { nombre: n.trim().to_string(), claves: HashMap::new() });
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if let Some(var) = k.strip_prefix('$') {
            let v = poner(v, vars);
            vars.insert(var.to_string(), v);
        } else if k == "source" && actual.is_none() && hondo < 4 {
            leer(Path::new(&poner(v, vars)), vars, bloques, hondo + 1);
        } else if let Some(b) = actual.as_mut() {
            b.claves.insert(k.to_string(), poner(v, vars));
        }
    }
}

fn hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s, 16).ok()
}

/// Un color de hyprlang: rgb(RRGGBB), rgba(RRGGBBAA), rgba(r, g, b, a),
/// rgb(r, g, b) o 0xAARRGGBB.
pub fn color(s: &str) -> Option<Color> {
    let s = s.trim();
    let por = |v: u32, sh: u32| ((v >> sh) & 0xff) as f32 / 255.0;
    if let Some(h) = s.strip_prefix("0x") {
        let v = hex(h)?;
        return Some([por(v, 16), por(v, 8), por(v, 0), por(v, 24)]);
    }
    let (f, adentro) = s.split_once('(')?;
    let adentro = adentro.strip_suffix(')')?.trim();
    let partes: Vec<&str> = adentro.split(',').map(str::trim).collect();
    match (f.trim(), partes.as_slice()) {
        ("rgb", [h]) if h.len() == 6 => {
            let v = hex(h)?;
            Some([por(v, 16), por(v, 8), por(v, 0), 1.0])
        }
        ("rgba", [h]) if h.len() == 8 => {
            let v = hex(h)?;
            Some([por(v, 24), por(v, 16), por(v, 8), por(v, 0)])
        }
        ("rgb", [r, g, b]) => Some([r.parse::<f32>().ok()? / 255.0, g.parse::<f32>().ok()? / 255.0, b.parse::<f32>().ok()? / 255.0, 1.0]),
        ("rgba", [r, g, b, a]) => {
            Some([r.parse::<f32>().ok()? / 255.0, g.parse::<f32>().ok()? / 255.0, b.parse::<f32>().ok()? / 255.0, a.parse().ok()?])
        }
        _ => None,
    }
}

/// Un color o un degradado: "rgb(..) rgb(..) 45deg".
pub fn pintura(s: &str) -> Option<Pintura> {
    let mut colores = Vec::new();
    let mut angulo = 0.0;
    let mut resto = s.trim();
    while !resto.is_empty() {
        if let Some(i) = resto.find(')').filter(|_| resto.contains('(')) {
            let (c, r) = resto.split_at(i + 1);
            colores.push(color(c)?);
            resto = r.trim_start();
        } else {
            let (palabra, r) = resto.split_once(char::is_whitespace).unwrap_or((resto, ""));
            if let Some(g) = palabra.strip_suffix("deg") {
                angulo = g.parse().ok()?;
            } else {
                colores.push(color(palabra)?);
            }
            resto = r.trim_start();
        }
    }
    match colores.len() {
        0 => None,
        1 => Some(Pintura::Solido(colores[0])),
        _ => Some(Pintura::Degradado(colores, angulo)),
    }
}

fn par(s: &str) -> Option<(f32, f32)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().trim_end_matches('%').parse().ok()?, b.trim().trim_end_matches('%').parse().ok()?))
}

fn alinea(s: Option<&String>) -> Alinea {
    match s.map(|s| s.trim()) {
        Some("left" | "top") => Alinea::Inicio,
        Some("right" | "bottom") => Alinea::Fin,
        _ => Alinea::Centro,
    }
}

/// `cmd[update:1000] echo "…"` → Cmd; lo demás, texto fijo.
fn texto(s: &str) -> Texto {
    if let Some(r) = s.strip_prefix("cmd[")
        && let Some((opts, cmd)) = r.split_once(']')
    {
        let ms = opts.split(',').find_map(|o| o.trim().strip_prefix("update:")?.trim().parse::<u64>().ok()).unwrap_or(0);
        // nada más seguido que cada segundo: el reloj de la chapa no va más rápido
        let cada = Duration::from_millis(ms.max(1000));
        return Texto::Cmd { cmd: cmd.trim().to_string(), cada };
    }
    Texto::Fijo(s.to_string())
}

impl Bloque {
    fn s(&self, k: &str) -> Option<&String> {
        self.claves.get(k)
    }
    fn f(&self, k: &str, def: f32) -> f32 {
        self.s(k).and_then(|v| v.trim().parse().ok()).unwrap_or(def)
    }
    fn b(&self, k: &str, def: bool) -> bool {
        match self.s(k).map(|v| v.trim()) {
            Some("true" | "yes" | "on" | "1") => true,
            Some("false" | "no" | "off" | "0") => false,
            _ => def,
        }
    }
    fn color(&self, k: &str, def: Color) -> Color {
        self.s(k).and_then(|v| color(v).or_else(|| pintura(v).map(|p| p.primero()))).unwrap_or(def)
    }
    fn pintura(&self, k: &str, def: Color) -> Pintura {
        self.s(k).and_then(|v| pintura(v)).unwrap_or(Pintura::Solido(def))
    }
    fn lugar(&self) -> Lugar {
        let (x, y) = self.s("position").and_then(|p| par(p)).unwrap_or((0.0, 0.0));
        Lugar { x, y, h: alinea(self.s("halign")), v: alinea(self.s("valign")) }
    }
    fn z(&self) -> i32 {
        self.f("zindex", 0.0) as i32
    }
}

const BLANCO: Color = [1.0, 1.0, 1.0, 1.0];
const NEGRO: Color = [0.0, 0.0, 0.0, 1.0];

fn armar(bloques: Vec<Bloque>) -> Option<Estilo> {
    let mut fondo = Fondo { color: NEGRO, brillo: 0.8172, blur: 0.0 };
    let mut cosas: Vec<(i32, Cosa)> = Vec::new();
    for b in bloques {
        match b.nombre.as_str() {
            "background" => {
                fondo = Fondo {
                    color: b.color("color", NEGRO),
                    brillo: b.f("brightness", 0.8172).clamp(0.0, 2.0),
                    blur: b.f("blur_size", 8.0) * b.f("blur_passes", 0.0),
                }
            }
            "label" => {
                let Some(t) = b.s("text") else { continue };
                let sombra = (b.f("shadow_passes", 0.0) > 0.0).then(|| Sombra {
                    tam: b.f("shadow_size", 3.0),
                    pasadas: b.f("shadow_passes", 0.0) as u32,
                    color: b.color("shadow_color", NEGRO),
                });
                cosas.push((
                    b.z(),
                    Cosa::Texto(Etiqueta {
                        texto: texto(t),
                        color: b.color("color", BLANCO),
                        familia: b.s("font_family").cloned().unwrap_or_else(|| "Sans".into()),
                        tam: b.f("font_size", 16.0),
                        lugar: b.lugar(),
                        sombra,
                    }),
                ));
            }
            "shape" => {
                let (w, h) = b.s("size").and_then(|s| par(s)).unwrap_or((100.0, 100.0));
                cosas.push((
                    b.z(),
                    Cosa::Forma(Forma {
                        w,
                        h,
                        color: b.color("color", BLANCO),
                        redondeo: b.f("rounding", -1.0),
                        borde: b.f("border_size", 0.0),
                        borde_color: b.pintura("border_color", BLANCO),
                        lugar: b.lugar(),
                    }),
                ));
            }
            "input-field" => {
                let (w, h) = b.s("size").and_then(|s| par(s)).unwrap_or((400.0, 90.0));
                let check = b.pintura("check_color", [0.8, 0.53, 0.13, 1.0]);
                cosas.push((
                    b.z(),
                    Cosa::Entrada(Entrada {
                        w,
                        h,
                        borde: b.f("outline_thickness", 4.0),
                        exterior: b.pintura("outer_color", [0.07, 0.07, 0.07, 1.0]),
                        interior: b.color("inner_color", [0.78, 0.78, 0.78, 1.0]),
                        letra: b.color("font_color", [0.04, 0.04, 0.04, 1.0]),
                        puntos: b.f("dots_size", 0.25).clamp(0.05, 0.8),
                        espacio: b.f("dots_spacing", 0.15).clamp(-1.0, 1.0),
                        centrados: b.b("dots_center", true),
                        redondeo: b.f("rounding", -1.0),
                        check,
                        fail: b.pintura("fail_color", [0.8, 0.13, 0.13, 1.0]),
                        capslock: b.s("capslock_color").and_then(|v| pintura(v)),
                        placeholder: b.s("placeholder_text").cloned().unwrap_or_default(),
                        fail_text: b.s("fail_text").cloned().unwrap_or_else(|| "<i>$FAIL</i>".into()),
                        ocultar_vacio: b.b("fade_on_empty", true),
                        ocultar_letras: b.b("hide_input", false),
                        familia: b.s("font_family").cloned().unwrap_or_else(|| "Sans".into()),
                        lugar: b.lugar(),
                    }),
                ));
            }
            _ => {}
        }
    }
    if cosas.is_empty() {
        return None;
    }
    // estable: a igual zindex, en el orden del archivo
    cosas.sort_by_key(|(z, _)| *z);
    Some(Estilo { fondo, cosas: cosas.into_iter().map(|(_, c)| c).collect() })
}

/// El hyprlock.conf vivo (el del look de ahora). `CHAPA_ESTILO=archivo` usa
/// otro, para probar un estilo sin cambiar de look.
pub fn vivo() -> PathBuf {
    if let Some(p) = std::env::var_os("CHAPA_ESTILO") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(format!("{home}/.config/hypr/hyprlock.conf"))
}

/// Lee un hyprlock.conf. `extra`: variables puestas de antemano (para ver un
/// look que no es el actual: sus {{font}} ya cambiados por quien llama).
pub fn cargar(path: &Path, texto_extra: Option<&str>) -> Option<Estilo> {
    let mut vars = HashMap::new();
    vars.insert("HOME".to_string(), std::env::var("HOME").unwrap_or_default());
    let mut bloques = Vec::new();
    match texto_extra {
        // el texto ya viene (un look que no es el actual, o una prueba)
        Some(t) => leer_texto(t, &mut vars, &mut bloques, 0),
        None => leer(path, &mut vars, &mut bloques, 0),
    }
    armar(bloques)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de(texto: &str) -> Estilo {
        cargar(Path::new("/no"), Some(texto)).expect("estilo")
    }

    #[test]
    fn comentarios_y_gato_escapado() {
        assert_eq!(sin_comentario("a = b # nota"), "a = b ");
        assert_eq!(sin_comentario("t = <span foreground=\"##6f6d6a\">x</span>"), "t = <span foreground=\"#6f6d6a\">x</span>");
    }

    #[test]
    fn colores() {
        assert_eq!(color("rgb(FF0000)"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(color("rgba(0b0b0d00)").unwrap()[3], 0.0);
        assert_eq!(color("rgba(255, 255, 255, 0.5)"), Some([1.0, 1.0, 1.0, 0.5]));
        assert_eq!(color("0xff00ff00"), Some([0.0, 1.0, 0.0, 1.0]));
        assert_eq!(color("nada"), None);
        match pintura("rgb(FF0000) rgb(0000FF) 45deg").unwrap() {
            Pintura::Degradado(v, a) => assert_eq!((v.len(), a), (2, 45.0)),
            p => panic!("{p:?}"),
        }
        assert_eq!(pintura("rgb(000000)"), Some(Pintura::Solido([0.0, 0.0, 0.0, 1.0])));
    }

    #[test]
    fn variables_largas_primero_y_las_de_hyprlock_quedan() {
        let mut v = HashMap::new();
        v.insert("bg0".into(), "rgb(000000)".into());
        v.insert("bg0_a00".into(), "rgba(00000000)".into());
        assert_eq!(poner("$bg0_a00", &v), "rgba(00000000)");
        assert_eq!(poner("$bg0", &v), "rgb(000000)");
        assert_eq!(poner("hola $USER en $LAYOUT", &v), "hola $USER en $LAYOUT");
    }

    #[test]
    fn un_look_entero() {
        let e = de(r#"
$hi = rgb(F2F0ED)
$font = Mono
background {
    color = rgb(0b0b0d)
    brightness = 0.62
    blur_passes = 2
    blur_size = 4
}
label {
    text = cmd[update:1000] echo "$(date +%H)"
    color = $hi
    font_size = 150
    font_family = $font Bold
    position = 80, 250
    halign = left
    valign = bottom
    zindex = 2
    shadow_passes = 3
    shadow_size = 6
}
input-field {
    size = 280, 40
    outer_color = $hi rgb(000000) 45deg
    placeholder_text = <i>contraseña</i>
    halign = right
    valign = bottom
    position = -80, 80
}
"#);
        assert_eq!(e.fondo.brillo, 0.62);
        assert_eq!(e.fondo.blur, 8.0);
        assert_eq!(e.cosas.len(), 2);
        // zindex: la entrada (0) antes que la etiqueta (2)
        let Cosa::Entrada(i) = &e.cosas[0] else { panic!() };
        assert!(matches!(i.exterior, Pintura::Degradado(..)));
        assert_eq!(i.lugar, Lugar { x: -80.0, y: 80.0, h: Alinea::Fin, v: Alinea::Fin });
        let Cosa::Texto(t) = &e.cosas[1] else { panic!() };
        assert_eq!(t.familia, "Mono Bold");
        assert_eq!(t.texto, Texto::Cmd { cmd: r#"echo "$(date +%H)""#.into(), cada: Duration::from_secs(1) });
        assert!(t.sombra.is_some());
    }

    #[test]
    fn sin_nada_que_dibujar_no_hay_estilo() {
        assert!(cargar(Path::new("/no"), Some("general {\n hide_cursor = true\n}\n")).is_none());
    }

    #[test]
    fn el_vivo_se_entiende() {
        // el hyprlock.conf de verdad del look de ahora (si hay)
        if vivo().exists() {
            assert!(cargar(&vivo(), None).is_some());
        }
    }
}
