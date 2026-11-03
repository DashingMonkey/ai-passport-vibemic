#!/usr/bin/env python3
"""Shared path-safety helpers for the release tools.

`archive_firmware.py` 与 `verify_firmware.py` 处理同一份不可信输入(构建目录里的
flash_args 与文件名),加固级别必须一致:旧实现里 archive 侧会拒绝 `../` 与符号
链接,而 verify 侧直接 `build_dir / name`,能 stat/读取构建目录之外的文件。
把判定放在这里,两个工具共用一套规则。
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

# macOS 把系统临时目录暴露成这两个别名;只解析这两个精确别名,不解析任意用户
# 创建的符号链接。
SYSTEM_DIRECTORY_ALIASES: dict[Path, Path] = (
    {Path("/tmp"): Path("/private/tmp"), Path("/var"): Path("/private/var")}
    if sys.platform == "darwin" else {}
)


def safe_path(value: str | Path, aliases: dict[Path, Path] | None = None) -> Path:
    """Reject traversal and every existing symlink component before filesystem use.

    `aliases` 允许调用方(archive_firmware)注入自己的系统目录别名表,
    以便测试替换;默认用本模块的 [`SYSTEM_DIRECTORY_ALIASES`]。
    """
    table = SYSTEM_DIRECTORY_ALIASES if aliases is None else aliases
    raw = os.fspath(value)
    if (
        not raw
        or "\x00" in raw
        or (os.name != "nt" and "\\" in raw)
        or ".." in Path(raw).parts
    ):
        raise ValueError(f"unsafe path: {raw!r}")
    path = Path(raw).absolute()
    for component in (*reversed(path.parents), path):
        if component.is_symlink():
            system_target = table.get(component)
            if system_target is not None and component.resolve() == system_target:
                return safe_path(system_target / path.relative_to(component), table)
            raise ValueError(f"symlink is not allowed: {component}")
    return path


def safe_relative_name(name: str) -> None:
    """Reject artifact names that are absolute, traversing, or shell-hostile."""
    if (
        not name
        or any(part in ("", ".", "..") for part in name.split("/"))
        or any(char in name for char in "\\:$`;&|<>\x00")
        or any(ord(char) < 32 for char in name)
    ):
        raise ValueError(f"unsafe artifact path: {name!r}")
