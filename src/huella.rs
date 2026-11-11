//! La huella, a la par de la contraseña: un hilo que le pide a fprintd un dedo
//! (`fprintd-verify`, que habla con fprintd por D-Bus como lo hace pam_fprintd)
//! y avisa qué pasó. Sin lector, sin fprintd o sin dedos registrados, se
//! despide callado y la chapa queda solo con contraseña.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay_client_toolkit::reexports::calloop::channel::Sender;

/// Ruta fija: lo que decide si se abre no sale de lo que haya en el PATH.
const VERIFY: &str = "/usr/bin/fprintd-verify";
/// Dedos que no coinciden antes de pedir la contraseña (como el cel).
pub const INTENTOS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Aviso {
    /// el lector está escuchando
    Lista,
    /// coincidió: abrir
    Si,
    /// no coincidió (va a escuchar otra vez, o ya no si fue el último)
    No,
    /// sin huella: no hay lector, ni dedos, o se acabaron los intentos
    Fuera,
}

#[derive(Debug, PartialEq)]
enum Resultado {
    Si,
    No,
    /// el dedo mal puesto: fprintd sigue escuchando solo
    Otra,
    /// el lector no se pudo usar (ocupado, desconectado…)
    Falla,
}

/// "Verify result: verify-match (done)" → Si. Lo demás que imprime, nada.
fn leer(linea: &str) -> Option<Resultado> {
    let r = linea.trim().strip_prefix("Verify result: ")?.split_whitespace().next()?;
    Some(match r {
        "verify-match" => Resultado::Si,
        "verify-no-match" => Resultado::No,
        "verify-retry-scan" | "verify-swipe-too-short" | "verify-finger-not-centered" | "verify-remove-and-retry" => {
            Resultado::Otra
        }
        _ => Resultado::Falla,
    })
}

/// El hilo de la huella. Al soltarlo (la chapa se abre por contraseña) mata el
/// `fprintd-verify` que esté corriendo, para que fprintd suelte el lector.
pub struct Huella {
    hijo: Arc<Mutex<Option<Child>>>,
}

impl Huella {
    pub fn empezar(tx: Sender<Aviso>) -> Huella {
        let hijo: Arc<Mutex<Option<Child>>> = Arc::default();
        let compartido = hijo.clone();
        std::thread::spawn(move || escuchar(&compartido, &tx));
        Huella { hijo }
    }

    pub fn parar(&self) {
        if let Some(mut c) = self.hijo.lock().ok().and_then(|mut h| h.take()) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Huella {
    fn drop(&mut self) {
        self.parar();
    }
}

fn escuchar(hijo: &Mutex<Option<Child>>, tx: &Sender<Aviso>) {
    let mut fallas = 0;
    let mut no = 0;
    loop {
        let Ok(mut c) = Command::new(VERIFY)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            let _ = tx.send(Aviso::Fuera);
            return;
        };
        let salida = c.stdout.take();
        match hijo.lock() {
            Ok(mut h) => *h = Some(c),
            Err(_) => return,
        }
        let mut fin = None;
        for linea in salida.into_iter().flat_map(|s| BufReader::new(s).lines().map_while(Result::ok)) {
            // ya tiene el lector y hay dedos: ahora sí, que la pantalla lo diga
            if linea.starts_with("Verify started") {
                let _ = tx.send(Aviso::Lista);
            }
            match leer(&linea) {
                Some(Resultado::Otra) | None => {}
                Some(r) => {
                    fin = Some(r);
                    break;
                }
            }
        }
        // ya terminó (o lo mataron al abrir con contraseña: entonces no hay hijo)
        let Some(mut c) = hijo.lock().ok().and_then(|mut h| h.take()) else { return };
        let _ = c.wait();

        match fin {
            Some(Resultado::Si) => {
                let _ = tx.send(Aviso::Si);
                return;
            }
            Some(Resultado::No) => {
                no += 1;
                let _ = tx.send(Aviso::No);
                if no >= INTENTOS {
                    let _ = tx.send(Aviso::Fuera);
                    return;
                }
                fallas = 0;
            }
            // sin resultado: sin dedos registrados, sin lector, o lo tiene otro
            // (un sudo esperando huella). Unas veces más, con calma, y adiós.
            _ => {
                fallas += 1;
                if fallas >= 3 {
                    let _ = tx.send(Aviso::Fuera);
                    return;
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_lo_que_dice_fprintd() {
        assert_eq!(leer("Verify result: verify-match (done)"), Some(Resultado::Si));
        assert_eq!(leer("Verify result: verify-no-match (done)"), Some(Resultado::No));
        assert_eq!(leer("Verify result: verify-finger-not-centered"), Some(Resultado::Otra));
        assert_eq!(leer("Verify result: verify-disconnected (done)"), Some(Resultado::Falla));
        assert_eq!(leer("Verify started!"), None);
        assert_eq!(leer("Using device /net/reactivated/Fprint/Device/0"), None);
        // que un "no-match" nunca se lea como "match"
        assert_ne!(leer("Verify result: verify-no-match"), Some(Resultado::Si));
    }
}
