//! Al abrir, el escritorio sale con el efecto del look (farula), como hyprlock
//! en LockScreen.sh: antes de soltar el bloqueo, el telón de farula se pone
//! debajo con el último cuadro de la chapa (bajo el bloqueo no se ve el
//! relevo) y, ya abierto, se revela.
//!
//! Nunca detiene la apertura: sin farula, con farula.off o si tarda más de
//! ESPERA, se abre sin telón.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ESPERA: Duration = Duration::from_millis(1500);

/// farula en el PATH (LockScreen.sh le pone ~/.local/bin) o en ~/.local/bin.
fn farula() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin"));
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .chain(home)
        .map(|d| d.join("farula"))
        .find(|f| f.is_file())
}

/// Un PPM (P6), lo que farula lee con --image.
fn ppm(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.extend_from_slice(rgb);
    out
}

/// Pone el telón con esta imagen (`w`×`h`, RGB). true: quedó puesto y hay que
/// `revelar` después de abrir.
pub fn poner(w: u32, h: u32, rgb: &[u8]) -> bool {
    let Some(farula) = farula() else { return false };
    if rgb.len() != (w * h * 3) as usize {
        return false;
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let img = dir.join("chapa-telon.ppm");
    if std::fs::write(&img, ppm(w, h, rgb)).is_err() {
        return false;
    }
    let Ok(mut freeze) = Command::new(&farula)
        .args(["freeze", "--hold", "--image"])
        .arg(&img)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        let _ = std::fs::remove_file(&img);
        return false;
    };
    // `freeze` vuelve cuando el telón ya está en pantalla
    let empezo = Instant::now();
    let puesto = loop {
        match freeze.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if empezo.elapsed() < ESPERA => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = freeze.kill();
                let _ = freeze.wait();
                // el telón quizá todavía sale (con --hold no se quita solo): que
                // se revele también si aparece tarde
                let _ = Command::new("sh")
                    .arg("-c")
                    .arg(r#""$0" reveal; sleep 2; "$0" reveal"#)
                    .arg(&farula)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                break false;
            }
        }
    };
    // farula ya la leyó (o no va a salir)
    let _ = std::fs::remove_file(&img);
    puesto
}

/// Abre el telón con el efecto del look. No espera la animación.
pub fn revelar() {
    if let Some(farula) = farula() {
        let _ = Command::new(farula).arg("reveal").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ppm_bien_formado() {
        let p = super::ppm(2, 1, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&p[..11], b"P6\n2 1\n255\n");
        assert_eq!(&p[11..], &[1, 2, 3, 4, 5, 6]);
    }
}
