# T7 Patch — migración a Rust

Aplicación de escritorio y DLL para **Black Ops III / T7 de 64 bits**, migradas a Rust. La ventana permite cambiar nombre, contraseña y filtro de amigos, y detecta el juego para activar el parche automáticamente. MinHook continúa siendo una dependencia nativa escrita en C, utilizada mediante FFI y gestionada por Cargo.

**Estado: port experimental compilable.** La interfaz y el mecanismo de carga se han comprobado localmente; la carga remota se prueba en un proceso auxiliar propio. La ejecución completa dentro de BO3 y la compatibilidad real con Wine/Proton necesitan pruebas en esos entornos.

## Usar la aplicación

1. Mantén **`dist/t7patch.exe` y `dist/t7patch.dll` en la misma carpeta**.
2. Abre `t7patch.exe` antes de iniciar BO3. La ventana mostrará `No game process found.`
3. Ajusta **Change Name**, **Network Password** y **Friends Only**. Los cambios se guardan automáticamente.
4. Inicia BO3 normalmente. La aplicación detecta `BlackOps3.exe`, carga la DLL y espera a que los objetos del juego estén disponibles.
5. `Patch active` aparece únicamente después de recibir la confirmación de la DLL. Si el texto de un error no cabe, haz clic en la barra de estado para leerlo completo.

El proceso se comprueba de nuevo mediante su handle antes de cargar la DLL, y su vida útil se sigue con ese handle para evitar confundir un PID reutilizado. Si cierras el juego, la aplicación vuelve a esperar al siguiente inicio. Abrir una segunda ventana trae al frente la existente.

Cerrar la aplicación deja activo el parche que ya se instaló en el juego. Al volver a abrirla se reconoce la DLL cargada. Los fallos definitivos de instalación y la desactivación explícita requieren reiniciar BO3; el estado «juego todavía no preparado» se reintenta automáticamente.

La interfaz conserva el estilo oscuro y los controles de la captura de referencia. El pie identifica al autor y `Learn more` abre el repositorio original de T7 Patch.

## Compilar

En Windows, con Rust y Visual Studio Build Tools (C++ x64 y Windows SDK):

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build.ps1
```

Salida:

```text
dist/t7patch.exe
dist/t7patch.dll
```

El script compila y copia los dos binarios sin sobrescribir `t7patch.conf`. La opción de PowerShell afecta solo al proceso que ejecuta ese script. También puedes compilar directamente:

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
```

Cargo genera `target/x86_64-pc-windows-msvc/release/t7patch-launcher.exe` y `t7patch.dll`. El ejecutable se llama `t7patch-launcher` dentro de Cargo para evitar colisiones de archivos PDB con la biblioteca; la distribución lo presenta como `t7patch.exe`.

Cargo descarga las dependencias indicadas en `Cargo.lock` y compila MinHook a través de `minhook-sys`, con el runtime correspondiente a la compilación Rust. No se necesitan bibliotecas `.lib` locales ni proyectos de Visual Studio para el parche; las herramientas C++ se utilizan para compilar la dependencia MinHook.

## Integración con el cargador

La DLL conserva los seis exports de la API original:

| Export | Función |
| --- | --- |
| `EnableInjectorlessInstall()` | Habilita la configuración por archivo; ya está activada de forma predeterminada en el original y en este port. |
| `zbr_run_gamemode_lui(input)` | Instala el parche si el hash de `input` coincide con `serious_anticrash_2023`. |
| `SetFriendsOnly(bool)` | Cambia el filtro de amigos. |
| `SetPlayerName(const char*)` | Cambia el nombre; admite hasta 15 bytes. |
| `SetNetworkPassword(const char*)` | Configura la contraseña de red; una cadena vacía o `NULL` la desactiva. |
| `Unload()` | Detiene el trabajador, deshabilita hooks y restaura las modificaciones registradas. |

Además, `T7PatchStart(void*)` implementa la API v1 del nuevo ejecutable: recibe una estructura sin punteros internos, con ruta UTF-16 absoluta del archivo de configuración, y devuelve el estado y un mensaje. Su firma es compatible con un punto de entrada de hilo Windows x64. Declaraciones C/C++: [`include/t7patch.h`](include/t7patch.h).

`DllMain` no instala el parche automáticamente. El nuevo ejecutable utiliza `LoadLibraryW` dentro del proceso de BO3 y después `T7PatchStart`, **fuera de `DllMain`**. Resuelve la dirección remota del módulo que contiene `LoadLibraryW` y el RVA del export de la DLL, sin asumir bases de módulos iguales entre procesos. Consulta el estado aproximadamente cada dos segundos.

Los cargadores anteriores pueden seguir usando `zbr_run_gamemode_lui` cuando Steam y el lobby estén listos. Una llamada temprana se limita a registrar que el juego aún no está preparado: no consume el intento de instalación. Los ajustes del archivo se aplican al instalar y cuando cambia su contenido.

La instalación comprueba la huella PE y las cantidades de patrones de integridad. Los errores se emiten mediante `OutputDebugStringA` con el prefijo `T7 Patch Rust:`.

### Versiones contempladas

`src/game_build.rs` conserva las huellas del proyecto original, sin obtener offsets nuevos:

| Identificador del original | PE TimeDateStamp | SizeOfImage admitido |
| --- | --- | --- |
| February2026 | `0x693D731E` | `0x1D74AC00`, `0x1D74B000` |
| September2026 | `0x6A7B6355` | `0x1D75BC00`, `0x1D75C000` |

Para septiembre se aplica el desplazamiento `-0x6C0` en el intervalo RVA `[0x1D29C20, 0x2EFF000)`. Estos datos proceden del proyecto original; todavía no se han validado contra una ejecución del juego durante esta migración.

## Configuración

Con el nuevo ejecutable, `t7patch.conf` se crea **junto al `.exe`**. Su ruta absoluta se comunica a la DLL, aunque BO3 tenga otro directorio de trabajo. Los cargadores antiguos que no suministren esa ruta mantienen el archivo relativo al directorio de trabajo del juego:

```ini
playername=Unknown Soldier
isfriendsonly=1
networkpassword=
```

La ventana guarda las ediciones tras una pausa breve al escribir; la casilla se guarda inmediatamente. La DLL comprueba el contenido aproximadamente cada segundo. La escritura publica un archivo completo mediante reemplazo, para que el lector no consuma una edición parcial. Un error de escritura o un valor inválido se muestra en la ventana y conserva el archivo anterior.

Se admiten finales de línea LF/CRLF, valores con `=` y una última línea sin salto de línea. Los límites son 15 bytes para el nombre y 1023 para la contraseña del archivo; no son límites de caracteres Unicode. La aplicación y el juego necesitan poder acceder a esa carpeta y tener niveles de permisos compatibles.

Los hashes respetan el `char` con signo de MSVC y el tratamiento ASCII de mayúsculas del original, incluidos los vectores de bytes altos comprobados con su implementación C++.

## Windows y Wine/Proton

Se genera **la misma DLL PE64 de Windows** para ambos. En Wine/Proton debe cargarse dentro del proceso de BO3, con el runtime Visual C++ x64 disponible en el prefijo. No se genera una biblioteca `.so`.

Para la detección automática, el `.exe` y BO3 deben ejecutarse en **el mismo prefijo y entorno Wine/Proton**. Ejecutar el lanzador con un Wine o prefijo diferente no le da acceso a los procesos del juego. La compilación actual utiliza APIs de escritorio de Windows 10 o posterior.

El manejador incluye:

- La ruta del puntero de `KiUserExceptionDispatcher` reconocida por el original.
- La secuencia de 27 bytes específica de Wine del código original, trasladada a `src/exceptions.rs`.
- Un manejador vectored de Windows como alternativa para las firmas no reconocidas; el original dejaba esa alternativa como `TODO`.

La ruta Wine está implementada, **pero no se ha ejecutado en Wine/Proton aquí**. Las firmas internas de `ntdll` y las llamadas a las funciones del juego requieren validación de ejecución.

## Desactivación y vida útil

`Unload()` es una **desactivación lógica**, no una descarga física. Se espera al trabajador antes de restaurar hooks. La DLL permanece fijada al proceso y conserva trampolines, la tabla virtual clonada y el manejador de excepciones en modo inactivo, para que las llamadas que estaban en curso no salten a código liberado. El manejador todavía reconoce el puntero centinela que otro hilo pudo haber leído antes de la restauración.

Como en el original, los parches de integridad permanecen hasta cerrar el proceso tras una instalación correcta. Los nombres publicados a Steam tienen almacenamiento inmutable que permanece válido. Los nombres del jugador, la semilla modificada y los valores de dvars configurados tampoco se deshacen. Reinicia BO3 para volver a instalar después de `Unload()` o de un fallo de instalación. No se reclama una restauración completa del estado del juego.

## Módulos

| Funcionalidad | Módulos en `src/` |
| --- | --- |
| Exports, ciclo de vida y excepciones | `lib.rs`, `runtime.rs`, `exceptions.rs` |
| Aplicación y ventana Win32 | `bin/t7patch.rs`, `launcher/ui.rs` |
| Detección, carga y estados | `launcher/mod.rs`, `launcher/process.rs`, `launcher_api.rs` |
| Formato de configuración compartido | `settings.rs` |
| Detección de versiones y traducción de direcciones | `game_build.rs` |
| Hashes | `hashing.rs` |
| Estructuras compatibles con el juego | `structs.rs` |
| Hooks | `hooks.rs` |
| Protecciones, paquetes y configuración | `protection.rs`, `packets.rs`, `config.rs` |
| Parches de integridad | `arxan.rs` |
| Acceso a memoria y FFI | `memory.rs`, `minhook.rs`; API de Windows mediante `windows-sys` |

Las ramas de `SPOOF_UNLOCK_ALL` y `SPOOF_SKIP_CWL` estaban desactivadas en el proyecto suministrado. Se mantiene ese comportamiento: no se instalan los hooks que únicamente reenviaban esas llamadas sin modificarlas. Los hooks de caché de nombres de script comentados en `ApplyHooks` tampoco se activan; la ruta de respuesta de menú sí valida sus índices.

## Verificar

```powershell
cargo fmt -- --check
cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo test --locked --target x86_64-pc-windows-msvc
cargo build --release --locked --target x86_64-pc-windows-msvc
cargo run --release --locked --target x86_64-pc-windows-msvc --example smoke -- target/x86_64-pc-windows-msvc/release/t7patch.dll
```

El ejemplo `smoke` prueba la carga y los exports en un proceso que **no es BO3**; no prueba los offsets del juego. Detalles y pruebas de ejecución pendientes: [`docs/VALIDATION.md`](docs/VALIDATION.md).

Las fuentes C++ antiguas y la herramienta auxiliar de comparación se retiraron tras obtener los vectores de referencia. Las pruebas Rust conservan esos valores y las comprobaciones de ABI. `include/t7patch.h` describe la API para cargadores C/C++. La migración no cambia la autoría del parche original ni las licencias de sus dependencias.
