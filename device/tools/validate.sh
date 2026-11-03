#!/usr/bin/env bash
set -euo pipefail

mode="${1:---all}"
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

# Python 解释器:POSIX 上通常只有 python3,Windows(Git Bash / MSYS)上往往是 python。
# Windows 还有第三种:Microsoft Store 的 python3 重定向 stub(AppInstaller),
# 它"存在"但只会静默失败(退出码 49、零输出)——所以光看 command -v 不够,
# 必须试跑一次;否则静态门禁会在第一个 python 脚本上无声挂掉。
if [[ -z "${PYTHON:-}" ]]; then
    if command -v python3 >/dev/null 2>&1 && python3 -c "" 2>/dev/null; then
        PYTHON=python3
    elif command -v python >/dev/null 2>&1; then
        PYTHON=python
    else
        echo "ERROR: neither python3 nor python is available." >&2
        exit 1
    fi
fi

usage() {
    echo "Usage: $0 [--all|--static|--firmware]" >&2
}

# 出货代码(vibemic_app.c + vm_*.c)在 IDF 之外本来一次都不会被编译:
# main/CMakeLists.txt 只把它们编进固件,而下面的宿主机测试全打在 demo_*.c 上。
# 这里用 tests/idf_stubs 的桩做 `-fsyntax-only`,让 API 漂移/笔误当场暴露;
# 不参与任何构建的 demo/main/ui_pixel 源文件也顺带检查,避免静默腐烂。
check_firmware_sources_syntax() {
    local cc="$1"
    local -a shipping=(
        main/vibemic_app.c
        main/vm_audio.c
        main/vm_audio_codec.c
        main/vm_backlight.c
        main/vm_batt_icon.c
        main/vm_buttons.c
        main/vm_hid.c
        main/vm_keymap.c
        main/vm_link.c
        main/vm_profile.c
        main/vm_protocol.c
    )
    # 这些文件既不在固件构建里,也没有任何测试 include(见 docs/development/
    # engineering/build-and-test.md 的覆盖说明)——语法检查是唯一的护栏。
    local -a unbuilt=(
        main/main.c
        main/demo_audio.c
        main/demo_battery.c
        main/demo_ble.c
        main/demo_button.c
        main/demo_display.c
        main/demo_low_power.c
        main/demo_navigation.c
        main/demo_radio.c
        main/demo_wifi.c
        main/ui_pixel.c
        main/ui_pixel_math.c
    )
    local src
    for src in "${shipping[@]}"; do
        "${cc}" -std=c11 -Wall -Wextra -Werror -fsyntax-only \
            -Itests/idf_stubs -Itests/bsp_stubs -Icomponents/bsp/include -Imain \
            "${src}"
    done
    for src in "${unbuilt[@]}"; do
        # 这些页面用 tests/demo_stubs 的桩(BSP 叫法也来自桩,刻意保持与
        # demo_stubs 自身一致);检查目的是"还在不在编译",不是 BSP 契约一致性
        # ——出货代码那一组才是拿真实 BSP 头 + idf_stubs 查的。
        "${cc}" -std=c11 -Wall -Wextra -Werror -fsyntax-only \
            -Itests/demo_stubs -Itests/bsp_stubs -Icomponents/bsp/include -Imain \
            "${src}"
    done
    echo "Firmware sources: syntax OK (shipping + retained demo sources)"
}

run_static_checks() (
    local test_dir
    local cc="${CC:-cc}"

    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tools/check_repo.py

    test_dir="$(mktemp -d "${TMPDIR:-/tmp}/ai-passport-host-tests.XXXXXX")"
    # 任何一步失败都要清掉临时目录(set -e 下原来的收尾 rm 根本走不到)。
    trap 'rm -rf -- "${test_dir}"' EXIT

    "${cc}" -std=c11 -Wall -Wextra -Werror -Imain \
        tests/test_vm_protocol.c main/vm_protocol.c \
        -o "${test_dir}/test_vm_protocol"
    "${test_dir}/test_vm_protocol"
    for runtime in link audio; do
        "${cc}" -std=c11 -Wall -Wextra -Werror -ffunction-sections -fdata-sections \
            -Itests/idf_stubs -Itests/bsp_stubs -Icomponents/bsp/include -Imain \
            "tests/test_vm_${runtime}_runtime.c" main/vm_protocol.c main/vm_audio_codec.c \
            -Wl,--gc-sections -o "${test_dir}/test_vm_${runtime}_runtime"
        "${test_dir}/test_vm_${runtime}_runtime"
    done
    "${cc}" -std=c11 -Wall -Wextra -Werror -Imain \
        tests/test_vm_audio_codec.c main/vm_audio_codec.c \
        -o "${test_dir}/test_vm_audio_codec"
    "${test_dir}/test_vm_audio_codec"
    # 按键映射(出货代码里第一个有 host test 的纯逻辑模块)。
    "${cc}" -std=c11 -Wall -Wextra -Werror \
        -Itests/bsp_stubs -Icomponents/bsp/include -Imain \
        tests/test_vm_keymap.c main/vm_keymap.c \
        -o "${test_dir}/test_vm_keymap"
    "${test_dir}/test_vm_keymap"
    # 空闲背光策略(纯逻辑,不依赖 BSP/IDF:门限边界、busy 抑制、时间戳回绕)。
    "${cc}" -std=c11 -Wall -Wextra -Werror -Imain \
        tests/test_vm_backlight.c main/vm_backlight.c \
        -o "${test_dir}/test_vm_backlight"
    "${test_dir}/test_vm_backlight"
    # 电池图标档位(百分比 → 图标/颜色的边界;图标字节也要锁)。
    "${cc}" -std=c11 -Wall -Wextra -Werror -Imain \
        tests/test_vm_batt_icon.c main/vm_batt_icon.c \
        -o "${test_dir}/test_vm_batt_icon"
    "${test_dir}/test_vm_batt_icon"
    "${cc}" -std=c11 -Wall -Wextra -Werror -Imain \
        tests/test_ui_pixel_math.c main/ui_pixel_math.c \
        -o "${test_dir}/test_ui_pixel_math"
    "${test_dir}/test_ui_pixel_math"
    "${cc}" -std=c11 -Wall -Wextra -Werror -Imain \
        tests/test_demo_navigation.c main/demo_navigation.c \
        -o "${test_dir}/test_demo_navigation"
    "${test_dir}/test_demo_navigation"
    "${cc}" -std=c11 -Wall -Wextra -Werror -Icomponents/bsp/src \
        tests/test_bsp_display_rounding.c components/bsp/src/bsp_display_rounding.c \
        -o "${test_dir}/test_bsp_display_rounding"
    "${test_dir}/test_bsp_display_rounding"
    "${cc}" -std=c11 -Wall -Wextra -Werror -Icomponents/bsp/src \
        tests/test_bsp_es8311_sleep_check.c components/bsp/src/bsp_es8311_sleep_check.c \
        -o "${test_dir}/test_bsp_es8311_sleep_check"
    "${test_dir}/test_bsp_es8311_sleep_check"
    "${cc}" -std=c11 -Wall -Wextra -Werror \
        -Itests/bsp_stubs -Icomponents/bsp/include \
        tests/test_bsp_button.c -o "${test_dir}/test_bsp_button"
    "${test_dir}/test_bsp_button"
    "${cc}" -std=c11 -Wall -Wextra -Werror \
        -Itests/bsp_stubs -Icomponents/bsp/include \
        tests/test_bsp_lvgl_init.c components/bsp/src/bsp_display_rounding.c \
        -o "${test_dir}/test_bsp_lvgl_init"
    "${test_dir}/test_bsp_lvgl_init"
    "${cc}" -std=c11 -Wall -Wextra -Werror \
        -Itests/audio_stubs -Icomponents/bsp/include -Icomponents/bsp/src \
        tests/test_bsp_audio_recovery.c components/bsp/src/bsp_es8311_sleep_check.c \
        -o "${test_dir}/test_bsp_audio_recovery"
    "${test_dir}/test_bsp_audio_recovery"
    for demo in audio low_power ble wifi; do
        "${cc}" -std=c11 -Wall -Wextra -Werror \
            -ffunction-sections -fdata-sections -Itests/demo_stubs -Imain \
            "tests/test_demo_${demo}_runtime.c" -Wl,--gc-sections \
            -o "${test_dir}/test_demo_${demo}_runtime"
        "${test_dir}/test_demo_${demo}_runtime"
    done
    check_firmware_sources_syntax "${cc}"
    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tests/test_deep_sleep_contract.py
    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tests/test_check_repo.py
    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tests/test_verify_firmware.py
    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tests/test_archive_firmware.py
    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tests/test_install_passport_skills.py
    echo "Host tests: PASS"
)

run_firmware_checks() (
    local validation_build_dir
    local idf_version

    if ! command -v idf.py >/dev/null 2>&1; then
        echo "ERROR: idf.py is not available; activate ESP-IDF 5.5.3 first." >&2
        return 1
    fi
    # 文档要求 5.5.3;只查"命令存在"会让别的 IDF 版本悄悄跑进来。
    idf_version="$(idf.py --version 2>/dev/null | tr -d '\r')"
    case "${idf_version}" in
        *v5.5.3*|*5.5.3*) ;;
        *) echo "ERROR: ESP-IDF ${idf_version:-unknown} found, need 5.5.3." >&2; return 1 ;;
    esac

    validation_build_dir="$(mktemp -d "${TMPDIR:-/tmp}/ai-passport-firmware.XXXXXX")"
    trap 'case "${validation_build_dir}" in */ai-passport-firmware.*) rm -rf -- "${validation_build_dir}" ;; esac' EXIT

    SDKCONFIG_DEFAULTS="${repo_root}/sdkconfig.defaults" \
        idf.py -B "${validation_build_dir}" \
        -D "SDKCONFIG=${validation_build_dir}/sdkconfig" build
    idf.py -B "${validation_build_dir}" merge-bin \
        -o "${validation_build_dir}/FoloToy-AI-Passport-full.bin"
    "${PYTHON}" tools/verify_firmware.py "${validation_build_dir}"
    PYTHONDONTWRITEBYTECODE=1 "${PYTHON}" tools/archive_firmware.py create \
        "${validation_build_dir}" --archive-root "${repo_root}/build/firmware"
    mkdir -p "${repo_root}/build"
    # 不用 GNU install(Windows/MinGW 上常常没有),cp + chmod 等价且可移植。
    cp "${validation_build_dir}/FoloToy-AI-Passport-full.bin" \
       "${repo_root}/build/FoloToy-AI-Passport-full.bin"
    chmod 0644 "${repo_root}/build/FoloToy-AI-Passport-full.bin"
    echo "Firmware build: PASS"
)

cd "${repo_root}"
case "${mode}" in
    --all)
        run_static_checks
        run_firmware_checks
        ;;
    --static)
        run_static_checks
        ;;
    --firmware)
        run_firmware_checks
        ;;
    *)
        usage
        exit 2
        ;;
esac
