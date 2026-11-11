//! La contraseña, con PAM: el servicio `chapa` (/etc/pam.d/chapa) si existe;
//! si no, el de hyprlock, que ya viene en discada. Se corre en un hilo aparte
//! para que el dibujo y el teclado no se traben mientras PAM piensa (puede
//! tardar segundos cuando la contraseña está mal).

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;

const PAM_SUCCESS: c_int = 0;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_REFRESH_CRED: c_int = 0x0010;

#[repr(C)]
struct Message {
    style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct Response {
    resp: *mut c_char,
    retcode: c_int,
}

#[repr(C)]
struct Conv {
    conv: extern "C" fn(c_int, *mut *const Message, *mut *mut Response, *mut c_void) -> c_int,
    data: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const Conv, handle: *mut *mut c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_setcred(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
}

/// PAM pregunta (a veces más de una vez): a lo que pide sin eco, la
/// contraseña; a lo demás (avisos), nada.
extern "C" fn converse(n: c_int, msgs: *mut *const Message, out: *mut *mut Response, data: *mut c_void) -> c_int {
    if n <= 0 || msgs.is_null() || out.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    unsafe {
        let pass = &*(data as *const CString);
        let resp = libc::calloc(n as usize, std::mem::size_of::<Response>()) as *mut Response;
        if resp.is_null() {
            return PAM_CONV_ERR;
        }
        for i in 0..n as usize {
            let m = *msgs.add(i);
            if !m.is_null() && matches!((*m).style, PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON) {
                // PAM se queda con la copia y la libera
                (*resp.add(i)).resp = libc::strdup(pass.as_ptr());
            }
        }
        *out = resp;
    }
    PAM_SUCCESS
}

/// Cuánto cabe: con todo el lugar apartado desde el principio, la contraseña
/// nunca se copia a otro lado al crecer (y deja una copia vieja en memoria).
const LUGAR: usize = 512;

/// La contraseña mientras se escribe. Lo que se quita (borrar, Esc, al
/// soltarla) se pone en ceros, no nomás se olvida.
pub struct Secreto(String);

impl Secreto {
    pub fn new() -> Secreto {
        Secreto(String::with_capacity(LUGAR))
    }

    pub fn letras(&self) -> usize {
        self.0.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Lo que no cabe se ignora: crecer sería copiar.
    pub fn push_str(&mut self, t: &str) {
        if self.0.len() + t.len() <= LUGAR {
            self.0.push_str(t);
        }
    }

    pub fn pop(&mut self) {
        let antes = self.0.len();
        self.0.pop();
        let ahora = self.0.len();
        // los bytes de la letra quitada siguen en el buffer: ceros
        unsafe { libc::explicit_bzero(self.0.as_mut_ptr().add(ahora) as *mut c_void, antes - ahora) };
    }

    pub fn clear(&mut self) {
        unsafe { libc::explicit_bzero(self.0.as_mut_ptr() as *mut c_void, self.0.len()) };
        self.0.clear();
    }

    /// Para mandarla a revisar: `check` la borra al terminar.
    pub fn take(&mut self) -> String {
        std::mem::replace(&mut self.0, String::with_capacity(LUGAR))
    }
}

impl Drop for Secreto {
    fn drop(&mut self) {
        self.clear();
    }
}

fn service() -> &'static str {
    if std::path::Path::new("/etc/pam.d/chapa").exists() { "chapa" } else { "hyprlock" }
}

/// ¿Es la contraseña de `user`? Borra la contraseña al terminar.
pub fn check(user: &str, mut pass: String) -> bool {
    let ok = (|| {
        let svc = CString::new(service()).ok()?;
        let user = CString::new(user).ok()?;
        let secret = CString::new(pass.as_bytes()).ok()?;
        let conv = Conv { conv: converse, data: &secret as *const CString as *mut c_void };
        let mut h = ptr::null_mut();
        unsafe {
            if pam_start(svc.as_ptr(), user.as_ptr(), &conv, &mut h) != PAM_SUCCESS {
                return Some(false);
            }
            let r = pam_authenticate(h, 0);
            if r == PAM_SUCCESS {
                // como hyprlock: refrescar credenciales (kerberos y compañía)
                pam_setcred(h, PAM_REFRESH_CRED);
            }
            pam_end(h, r);
            // borrar la copia
            let raw = secret.into_raw();
            libc::explicit_bzero(raw as *mut c_void, libc::strlen(raw));
            drop(CString::from_raw(raw));
            Some(r == PAM_SUCCESS)
        }
    })()
    .unwrap_or(false);
    unsafe { libc::explicit_bzero(pass.as_mut_ptr() as *mut c_void, pass.len()) };
    ok
}

#[cfg(test)]
mod tests {
    use super::Secreto;

    #[test]
    fn secreto_no_crece_ni_deja_rastro() {
        let mut s = Secreto::new();
        let lugar = s.0.capacity();
        s.push_str("ñandú");
        s.pop();
        assert_eq!(s.letras(), 4);
        // la "ú" quitada quedó en ceros, más allá del largo
        let crudo = unsafe { std::slice::from_raw_parts(s.0.as_ptr(), 7) };
        assert_eq!(&crudo[5..], &[0, 0]);
        s.push_str(&"x".repeat(2000));
        assert_eq!(s.0.capacity(), lugar, "no se copió a otro lado");
        assert_eq!(s.letras(), 4);
        assert_eq!(s.take(), "ñand");
        assert!(s.is_empty());
    }

    #[test]
    fn mala_no_abre() {
        let user = std::env::var("USER").unwrap();
        assert!(!super::check(&user, "esta-no-es-la-contraseña-de-nadie".into()));
        assert!(!super::check("", String::new()));
    }
}
