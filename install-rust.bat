@echo off
REM Instala Rust toolchain GNU (no requiere VS C++ build tools).
REM Uso: doble click o ejecutar desde cmd/PowerShell con privilegios normales.

setlocal
set RUSTUP=C:\Users\Py\rustup-init.exe
if not exist "%RUSTUP%" (
  echo Descargando rustup-init.exe...
  powershell -Command "Invoke-WebRequest -Uri https://win.rustup.rs/x86_64 -OutFile %RUSTUP%"
)

echo Instalando toolchain GNU (sin modificar PATH global)...
"%RUSTUP%" -y --default-toolchain stable-x86_64-pc-windows-gnu --profile minimal --no-modify-path

echo.
echo Verificando instalacion...
"%USERPROFILE%\.cargo\bin\cargo.exe" --version
"%USERPROFILE%\.cargo\bin\rustc.exe" --version

echo.
echo OK. Ahora podes correr:
echo   cd C:\Users\Py\Documents\bot-polymarket-rust
echo   "%USERPROFILE%\.cargo\bin\cargo.exe" build --release
endlocal
