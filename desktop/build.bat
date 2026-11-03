@echo off
setlocal
cd /d "%~dp0"

echo ================================================================
echo   VibeMic desktop build (Tauri 2, standalone exe)
echo ================================================================
echo.

rem A user-level CARGO_TARGET_DIR redirects cargo's output away from target\,
rem so the artifacts listed below (and the exe this script starts) would be
rem missing or stale. Warn instead of silently "succeeding" with an old exe.
if defined CARGO_TARGET_DIR (
    echo [WARN] CARGO_TARGET_DIR is set to "%CARGO_TARGET_DIR%".
    echo        Build artifacts may not be under target\ - check that path
    echo        if the expected files are missing below.
    echo.
)

where npm >nul 2>nul
if errorlevel 1 (
    echo [ERROR] npm not found. Install Node.js LTS first.
    goto :fail
)

where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] cargo not found. Install Rust from https://rustup.rs
    goto :fail
)

rem 构建产物路径:第 3 步要用它做"文件锁有没有放开"的检查,所以在这里先定义好
set "APP_EXE=%~dp0target\release\VibeMic.exe"

rem ---- sherpa-onnx 引擎归档(离线构建需要,见 .cargo\config.toml 注释) ----
rem 版本号从 Cargo.lock 动态读取,升级 sherpa-onnx 后无需改本脚本。
set "ARCHIVE_DIR=%~dp0third_party\sherpa-onnx"
for /f "usebackq delims=" %%v in (`powershell -NoProfile -Command "([regex]::Match((Get-Content -Raw '%~dp0Cargo.lock'), 'name = \x22sherpa-onnx-sys\x22\r?\nversion = \x22([\d.]+)\x22').Groups[1].Value)"`) do set "SHERPA_VER=%%v"
if not defined SHERPA_VER (
    echo [ERROR] Cannot read sherpa-onnx-sys version from Cargo.lock.
    goto :fail
)
set "ARCHIVE_NAME=sherpa-onnx-v%SHERPA_VER%-win-x64-static-MT-Release-lib.tar.bz2"
set "ARCHIVE_URL=https://github.com/k2-fsa/sherpa-onnx/releases/download/v%SHERPA_VER%/%ARCHIVE_NAME%"
set "ARCHIVE_MIN_BYTES=20971520"
rem 上面的体积下限只挡明显残缺的下载;真正的判定是 :archive_ok 里的 tar 校验。

echo [1/6] Ensuring sherpa-onnx engine archive for the ASR build...
rem 两条路:离线通道(已解压好的预编译库)优先,其次是归档(下载或复用已校验的那份)。
rem 为什么必须校验:下载中断会在 third_party 留下半个 .tar.bz2(残包);bz2 是流式压缩,
rem 截断的文件会让 sherpa-onnx-sys 的 build script 在解压时报 "failed to unpack ...
rem 某个 .lib" —— 报错完全看不出真因,而脚本以前"看到文件存在就认为已经下好了",
rem 于是每次构建都在同一处失败。现在改成:先验,不通过就删掉重下。
call :detect_prebuilt_libs
if defined SHERPA_ONNX_LIB_DIR goto :archive_ready

call :archive_ok
if not errorlevel 1 (
    echo       %ARCHIVE_NAME% already present and verified.
    goto :archive_ready
)

if exist "%ARCHIVE_DIR%\%ARCHIVE_NAME%" (
    echo       Existing archive failed verification - removing it.
    del /f /q "%ARCHIVE_DIR%\%ARCHIVE_NAME%"
)
where curl >nul 2>nul
if errorlevel 1 (
    echo [ERROR] curl not found. Download manually:
    echo         %ARCHIVE_URL%
    echo         into %ARCHIVE_DIR%
    goto :fail
)
if not exist "%ARCHIVE_DIR%" mkdir "%ARCHIVE_DIR%"
echo       Downloading %ARCHIVE_NAME% - about 117MB, first time only...
rem 不用 -C -:残包已在上面删掉,带着断点续传只会把上一次的坏前缀留在文件里。
curl -L --fail --retry 3 --retry-delay 2 -o "%ARCHIVE_DIR%\%ARCHIVE_NAME%" "%ARCHIVE_URL%"
if errorlevel 1 (
    echo [ERROR] Download failed. Rerun build.bat to retry:
    echo         %ARCHIVE_URL%
    echo         into %ARCHIVE_DIR%
    goto :fail
)
call :archive_ok
if errorlevel 1 (
    echo [ERROR] The downloaded archive is incomplete or corrupt:
    echo         %ARCHIVE_DIR%\%ARCHIVE_NAME%
    echo         Delete that file and rerun build.bat, or download it manually:
    echo         %ARCHIVE_URL%
    goto :fail
)
echo       Download complete and verified.

:archive_ready


cd app

echo [2/6] Installing frontend dependencies...
rem npm has no frozen mode that reuses node_modules; plain install is
rem incremental and still respects package-lock.json.
call npm install --prefer-offline
if errorlevel 1 goto :fail

echo.
echo [3/6] Closing a running VibeMic instance (the exe must be unlocked for linking)...
rem taskkill's errorlevel tells whether anything was killed: 0 = killed, 128 = not found.
rem Release and debug builds share the exe name, so both get closed.
taskkill /F /IM VibeMic.exe >nul 2>nul
if not errorlevel 1 (
    echo VibeMic was running and has been closed.
) else (
    echo VibeMic is not running.
)
rem Clear the errorlevel taskkill leaves when nothing matched (128),
rem so later checks only see results of their own commands
ver >nul

rem 进程没了不等于文件锁立刻没了:exe 刚跑过时 Windows 还会占着映像一小会儿,
rem 这期间 cargo 链接会以 "failed to remove ... 拒绝访问" 收场,整轮 release 构建白跑
rem (实测踩到过:刚启动过的 exe 被占着,报错后 cargo 自己重试才写成功,但错误已经报出去)。
rem 注意 taskkill 的 errorlevel **分不出**"没进程"和"锁还在" —— 首次运行的机器上进程本来
rem 就不存在,所以这里无条件等,而不是只在"刚杀过进程"时才等。
rem 判据用"能不能删掉",因为只读检测看不出独占锁。删掉不影响构建:cargo 会重新生成。
rem ping-based delay works even when stdin is redirected; timeout.exe would bail out.
rem 注意:下面这些 echo 一律不要出现右括号 —— 它们在 if ( ... ) 块里,cmd 会在那个括号处
rem 提前结束块,报 "was unexpected at this time"。同理不要用 ^( ^) 之外的括号字符。
set "WAITED=0"
:wait_unlock
rem 先记这一轮已经等过的时间,再判锁 —— 否则"第一次就删得掉"会显示成等了 0 秒
set /a WAITED+=2
%SystemRoot%\System32\ping.exe -n 2 127.0.0.1 >nul
if not exist "%APP_EXE%" goto :unlocked
del /f /q "%APP_EXE%" >nul 2>nul
if not exist "%APP_EXE%" goto :unlocked
if %WAITED% lss 10 goto :wait_unlock
echo [ERROR] The previous VibeMic.exe is still locked after %WAITED%s:
echo         %APP_EXE%
echo         Something other than a running VibeMic holds it - antivirus
echo         scan, Explorer preview, or a debugger. Close it and rerun.
goto :fail

:unlocked
echo       Old exe released after about %WAITED%s.

echo.
echo [4/6] Type-checking frontend...
call npm run typecheck
if errorlevel 1 goto :fail

echo.
echo [5/6] Building frontend + Rust release. First run may take several minutes...
call npm run tauri build
if errorlevel 1 goto :fail

echo.
echo [6/6] Checking the built exe, then starting VibeMic...
rem 产物存在性校验:cargo 报成功不等于 exe 真的在,这里兜住"构建成功但产物缺失"。
if not exist "%APP_EXE%" (
    echo [ERROR] Cargo reported success, but the exe is missing:
    echo         %APP_EXE%
    echo         If CARGO_TARGET_DIR is set, the exe went to that directory
    echo         instead - unset it and rebuild.
    goto :fail
)
rem 带 --show-panel 启动:软件平时是"托盘常驻、面板不弹"(见 --show-panel 处的注释),
rem 但刚构建完还看不到任何窗口,会让人以为没启动成功 —— 这里直接把面板调出来,
rem 也顺带证明这次构建的产物真能跑起来(未连接设备时会在日志里持续重试,见 app.log)。
start "" "%APP_EXE%" --show-panel

echo.
echo ================================================================
echo   BUILD OK
echo ================================================================
echo VibeMic has been started (panel opened). Closing the panel only
echo hides it to the tray; quit from the tray icon's right-click menu.
echo.
echo App (double-click to run anytime):
echo   %APP_EXE%
echo.
echo No installer is produced: bundle.active is false in
echo app\src-tauri\tauri.conf.json. Set it to true to also get an
echo NSIS installer under target\release\bundle\nsis\.
echo.
goto :end

:fail
echo.
echo [FAILED] Build error. Check the messages above.
pause
exit /b 1

:end
pause
exit /b 0


rem ================= 子过程 =================
rem 下面两个子过程由 [1/6] 用 call 调用,判定结果一律用 errorlevel 返回(0 = 通过)。
rem cmd 的坑:子过程必须放在主流程走不到的位置(:end 的 exit /b 之后)并自己
rem exit /b,否则主流程会顺着往下执行;子过程内的 echo 若在 if 块里,同样不能出现
rem 右括号(见上面 [3/6] 那段注释)。

rem :detect_prebuilt_libs —— third_party 下已有解压好的预编译库时,把
rem SHERPA_ONNX_LIB_DIR 交给 sherpa-onnx-sys:它的 build script 最高优先级看这个
rem 变量(见 sherpa-onnx-sys 的 build.rs resolve_lib_dir),于是弱网/离线机器既不
rem 用下 117MB 归档,也不用每次重新解压 1GB 的库。库怎么来:成功构建一次后
rem target\sherpa-onnx-prebuilt\sherpa-onnx-v*-lib\lib 就是解压结果,把它拷到
rem third_party\sherpa-onnx\ 下即可(该目录被 .gitignore 忽略,不会进版本库)。
rem 用户自己设了 SHERPA_ONNX_LIB_DIR 时不覆盖,直接沿用他的。
:detect_prebuilt_libs
if defined SHERPA_ONNX_LIB_DIR goto :prebuilt_libs_report
if exist "%ARCHIVE_DIR%\lib\sherpa-onnx-core.lib" set "SHERPA_ONNX_LIB_DIR=%ARCHIVE_DIR%\lib"
if not defined SHERPA_ONNX_LIB_DIR for /d %%D in ("%ARCHIVE_DIR%\sherpa-onnx-*-lib") do if exist "%%D\lib\sherpa-onnx-core.lib" set "SHERPA_ONNX_LIB_DIR=%%D\lib"
:prebuilt_libs_report
if defined SHERPA_ONNX_LIB_DIR echo       Using prebuilt libs already unpacked: %SHERPA_ONNX_LIB_DIR%
exit /b 0

rem :archive_ok —— 归档存在且可信? errorlevel 0 = 可以交给构建。两层校验:
rem   1. 体积下限:挡住明显残缺的下载(例如几百 KB 的残包);
rem   2. tar 能列出目录:bz2 流完整、尾部索引可读 —— 这才是决定性的。
rem 刻意不把期望字节数钉死:归档名里的版本号是从 Cargo.lock 动态读的,升级
rem sherpa-onnx 后体积会变,钉死会让脚本陷入"删掉重下还是不过"的死循环。
:archive_ok
if not exist "%ARCHIVE_DIR%\%ARCHIVE_NAME%" exit /b 1
set "ARCHIVE_GOT="
for %%A in ("%ARCHIVE_DIR%\%ARCHIVE_NAME%") do set "ARCHIVE_GOT=%%~zA"
if not defined ARCHIVE_GOT exit /b 1
if %ARCHIVE_GOT% lss %ARCHIVE_MIN_BYTES% exit /b 1
rem tar 是 Windows 10 1803+ 自带的 bsdtar;没有它时只保留体积校验。
where tar >nul 2>nul
if errorlevel 1 exit /b 0
tar -tjf "%ARCHIVE_DIR%\%ARCHIVE_NAME%" >nul 2>nul
if errorlevel 1 exit /b 1
exit /b 0
