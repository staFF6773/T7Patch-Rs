# Validación del port

## Resultados locales

Verificado en Windows x64 con Rust/Cargo 1.98.1 y MSVC:

| Comprobación | Resultado |
| --- | --- |
| Compilación Release de EXE y DLL | Correcta, sin conflicto de CRT ni colisión de nombres PDB |
| Clippy con `-D warnings` | Correcto |
| Pruebas Rust | 13 de biblioteca y 5 del ejecutable aprobadas; 3 adicionales de módulos compartidos en el ejemplo |
| Comparación histórica con el C++ original | Aserciones ABI y vectores de hashes comprobados antes de retirar las fuentes antiguas |
| API de DLL | Se conservan los seis exports originales; se añade `T7PatchStart` para el ejecutable |
| Ejemplo `smoke` con la DLL Release | Carga, llamadas y rechazo de host desconocido correctos |
| Carga remota en un proceso auxiliar propio | DLL cargada, respuesta `UNSUPPORTED` recibida y módulo existente reconocido en una segunda conexión |
| Ventana Win32 | Creación de controles, bucle de mensajes y cierre comprobados; captura visual revisada |
| BO3 en Windows | Pendiente |
| Wine/Proton | Pendiente |

## Pruebas automáticas

- Huellas PE de las dos versiones y límites de traducción RVA.
- Tamaños, alineación y offsets de estructuras de mensajes, sesión y Steam.
- Hashes de 32/64 bits y FNV contra vectores obtenidos compilando el C++ suministrado.
- Configuración CRLF, límites de nombre, contraseñas vacías y última línea sin salto.
- Guardado y reemplazo de configuración con ruta Unicode; conservación del archivo anterior cuando una edición es inválida.
- Layout del mensaje de inicio, respuesta acotada y rechazo de versiones/argumentos incorrectos.
- Lectura de exports PE64; rechazo de tablas inválidas, exports reenviados y ordinales fuera de rango.
- Directivas de localización mal formadas y límites de claves de modelos UI.
- Límites de miembros de join y candidatos de heartbeat, incluyendo negativos, truncamientos y elementos adicionales.
- Hook real de una función de máquina creada en el proceso de prueba: original devuelve 7, hook devuelve 42, trampoline devuelve 7, desactivación vuelve a 7.

Los inspectores usan una copia del lector y almacenamiento local por llamada. Sus tests simulan la interfaz de serialización; no sustituyen una prueba del protocolo con los serializadores reales de BO3.

## Procedencia de los valores de referencia

Durante la migración se compiló una herramienta auxiliar que incluía el hashing y las estructuras del C++ original. Sus aserciones `static_assert` verificaron la ABI y su salida proporcionó los vectores de hashes, incluidos bytes no ASCII.

Las fuentes antiguas y esa herramienta se retiraron durante la limpieza del proyecto. Los vectores permanecen fijados en `src/hashing.rs`, y las comprobaciones de tamaños, alineación y offsets están en `src/structs.rs`. Ambas se ejecutan con `cargo test`; ya no hay un comando de comparación C++ en este proyecto.

## Prueba de carga

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
cargo run --release --locked --target x86_64-pc-windows-msvc --example smoke -- target/x86_64-pc-windows-msvc/release/t7patch.dll
```

Comprueba `LoadLibrary`, resolución de los seis exports, nombres y contraseñas, entradas nulas, desactivación repetida y rechazo de instalación en un ejecutable ajeno a BO3.

Para probar **la carga** en Wine, se puede copiar `release/examples/smoke.exe` junto con `release/t7patch.dll` a una máquina con Wine, y ejecutar:

```sh
wine ./smoke.exe 'Z:\ruta\absoluta\t7patch.dll'
```

La ruta Windows debe corresponder al archivo dentro de ese prefijo. Esta prueba de carga no instala el manejador de Wine, ya que el host es deliberadamente desconocido.

## Prueba del nuevo cargador

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
cargo build --release --locked --target x86_64-pc-windows-msvc --examples
.\target\x86_64-pc-windows-msvc\release\examples\loader_smoke.exe
```

`loader_smoke` crea su propio proceso `launcher_host.exe`, carga la DLL en él con el mecanismo real y espera una respuesta explícita de ejecutable no compatible. Repite la conexión para comprobar que reconoce el módulo ya cargado. El proceso auxiliar se cierra al terminar. Esta prueba no busca ni modifica BO3.

Para comprobar la ventana sin activar la detección del juego ni guardar ajustes:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build.ps1
.\dist\t7patch.exe --ui-smoke-test
```

La ventana se cierra sola tras aproximadamente dos segundos. Para obtener una captura recortada a esa ventana de prueba:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\capture-ui.ps1
```

La imagen se guarda en `target/launcher-preview.png`. Se revisó su apariencia oscura, bordes azules, campos, casilla de amigos, enlace y barra de estado.

El modo de prueba de UI no demuestra la activación dentro del juego. El protocolo del lanzador sí diferencia `WAITING`, `ACTIVE`, `UNSUPPORTED`, `FAILED`, `DEACTIVATED` y `BAD_REQUEST`; la confirmación `ACTIVE` solo se emite al completar la instalación de la DLL.

## Diferencias corregidas respecto al original

- `hkLobbyMsgRW_PrepReadMsg` ahora devuelve error cuando falla la lectura o no coincide el prefijo; el original terminaba en `return true` en ambos casos.
- La ruta de mensajes instantáneos usa los campos del `msg_t` inicializado, en lugar de interpretar los bytes del paquete como un `msg_t`.
- Los límites de arrays rechazan cantidades negativas, elementos extra y truncamientos en listas con tamaño declarado. Los errores de serialización se propagan.
- El índice de evento de `MenuResponseCached` se comprueba antes de acceder a la caché.
- Las cadenas se acotan antes de copiarlas. Se admite `NULL` para borrar la contraseña y se ignoran nombres nulos.
- Los hashes usan operaciones con desbordamiento definido, también en compilaciones Debug.
- Hay señal de parada y espera del trabajador; la instalación y desactivación se serializan.
- Se guardan bytes/punteros para restauración y se comprueban errores de MinHook y de escritura. Los fallos se registran mediante `OutputDebugStringA`.
- La DLL queda fijada al proceso para conservar callbacks/trampolines todavía en ejecución. No se vuelve a instalar en ese proceso después de desactivarla.
- La aplicación espera los objetos necesarios antes de consumir el intento de instalación. Los reintentos por arranque incompleto no se tratan como fallos definitivos.
- EXE y DLL comparten una ruta de configuración absoluta. Cerrar la ventana detiene su trabajador de detección; la DLL ya instalada conserva su trabajador y sus hooks.

Estas correcciones cambian casos del protocolo que el C++ trataba de forma inconsistente. La interoperabilidad con otro jugador usando la versión C++ debe comprobarse, especialmente al cambiar la contraseña durante una sesión.

## Pruebas que requieren BO3 — pendientes

Para cada ejecutable contemplado, tanto en Windows como en Wine/Proton:

1. Abrir `t7patch.exe` antes de iniciar BO3. Comprobar la transición desde `No game process found.` a espera de inicialización y finalmente `Patch active`, sin errores de patrones/MinHook. Repetir abriendo la aplicación cuando el juego ya está arrancado.
2. Entrar al menú, campaña, multijugador y Zombies. Verificar nombre, localización, modelos UI y creación de lobby.
3. Probar invitaciones de amigos, filtro de desconocidos, chat y transición entre lobbies.
4. Probar contraseña igual/diferente/vacía y cambio durante una sesión; comprobar los prefijos y el margen de 1500 ms del checksum anterior.
5. Reproducir en un entorno de prueba las correcciones de crashes conocidas y verificar registros/contexto. Los hashes PE por sí solos no demuestran que cada RVA sea correcto.
6. Desactivar durante el menú y comprobar la parada del trabajador y la restauración de las entradas modificadas. Cerrar el proceso para completar la liberación de recursos.
7. En Wine/Proton, identificar la versión usada y verificar específicamente el camino del dispatcher. La secuencia ensamblada procede del original y depende del layout de esa versión de `ntdll`.
8. Cambiar los campos de la ventana y comprobar su aplicación dentro del juego. Cerrar y reabrir la aplicación con BO3 activo; cerrar BO3 y volver a iniciarlo manteniendo la ventana abierta. Repetir en el mismo prefijo de Wine/Proton.

No se ha ejecutado el juego ni Wine/Proton durante la migración. No se afirma equivalencia funcional comprobada en esos entornos.
