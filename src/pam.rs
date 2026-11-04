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
    #[test]
    fn mala_no_abre() {
        let user = std::env::var("USER").unwrap();
        assert!(!super::check(&user, "esta-no-es-la-contraseña-de-nadie".into()));
        assert!(!super::check("", String::new()));
    }
}
