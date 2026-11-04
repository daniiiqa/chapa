# chapa

La chapa de discada: bloquea la sesión de Hyprland (ext-session-lock, la
misma vía que hyprlock) con el look del rice: la hora grande, la fecha y la
contraseña como puntos, con la fuente de kitty y los colores del look (disco).
La contraseña la revisa PAM (`/etc/pam.d/chapa`, o el de hyprlock si no hay).
Si chapa se cae, Hyprland deja la sesión bloqueada; nunca la abre.

```
chapa               bloquear
chapa --prueba N    bloquear y abrir sola a los N segundos (para probar)
```

Etapas: 1) bloqueo seguro con PAM · 2) wallpaper en video con libmpv (esta:
fluido, con VAAPI; sin EGL/mpv, fondo sólido) · 3) el estilo de cada look y
farula al abrir.

MIT.
