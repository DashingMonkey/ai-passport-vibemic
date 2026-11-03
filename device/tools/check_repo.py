#!/usr/bin/env python3
"""Dependency-free repository checks shared by local development and CI."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parent.parent
MARKDOWN_LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
CJK_RE = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff]")
SECRET_PATTERNS = {
    "GitHub token": re.compile(r"(?:ghp_|github_pat_)[A-Za-z0-9_]{20,}"),
    "AWS access key": re.compile(r"AKIA[0-9A-Z]{16}"),
    "private key": re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
}
# 设备二维码链接(任意路径,后接查询串);参数逐个判定,见 check_sensitive_content。
QR_LINK_RE = re.compile(r"https://ai-passport\.folotoy\.cn/[^\s)\"'`]*\?([^\s)\"'`]+)")
# 视为"已脱敏"的写法:`<...>` 占位符或 AAAAAA 掩码。
MASK_HINTS = ("<", "AAAAAA")


def looks_masked(value: str) -> bool:
    return any(hint in value for hint in MASK_HINTS)
ROOT_MARKDOWN_ALLOWLIST = {
    "AGENTS.md",
    "AGENTS.zh_CN.md",
    "CLAUDE.md",
    "CLAUDE.zh_CN.md",
    "README.md",
    "README.zh_CN.md",
}
# Register only concrete, vendored component directories, e.g. "components/foo".
# These exemptions never change the input to sensitive-content/conflict checks.
VENDORED_DOC_ROOTS: tuple[str, ...] = ()


def vendored_document_roots(errors: list[str]) -> tuple[Path, ...]:
    """Validate explicit repository-relative directories; reject symlink roots."""
    roots: list[Path] = []
    for name in VENDORED_DOC_ROOTS:
        if (
            not isinstance(name, str)
            or not name
            or "\\" in name
            or "\x00" in name
            or re.match(r"^[A-Za-z]:", name)
            or any(part in ("", ".", "..") for part in name.split("/"))
        ):
            errors.append(f"invalid VENDORED_DOC_ROOTS entry {name!r}: use a repository-relative directory")
            continue
        path = ROOT / name
        try:
            resolved = path.resolve(strict=True)
        except (OSError, RuntimeError):
            errors.append(f"invalid VENDORED_DOC_ROOTS entry {name!r}: directory does not resolve")
            continue
        if resolved != path or not resolved.is_relative_to(ROOT) or not resolved.is_dir():
            errors.append(f"invalid VENDORED_DOC_ROOTS entry {name!r}: require a real directory without symlinks")
            continue
        roots.append(resolved)
    return tuple(roots)


def is_vendored_document(path: Path, roots: tuple[Path, ...]) -> bool:
    """Require both the file's listed path and its target to stay in one root."""
    for root in roots:
        if not path.is_relative_to(root):
            continue
        try:
            if path.resolve().is_relative_to(root):
                return True
        except (OSError, RuntimeError):
            return False
    return False


def git_files() -> list[Path]:
    try:
        result = subprocess.run(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            # 固定用 UTF-8 解码:默认按平台 locale(cp936 等)会把非 ASCII 文件名
            # 解成乱码,ROOT / mangled 随即不存在 → 该文件被静默跳过,
            # 于是"本地 PASS、CI FAIL"。
            encoding="utf-8",
            errors="surrogateescape",
        )
    except FileNotFoundError as error:
        raise SystemExit("ERROR: git is not available; repository checks need git") from error
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"ERROR: git ls-files failed: {error}") from error
    return [ROOT / line for line in result.stdout.splitlines() if line]


def text_files() -> list[Path]:
    files: list[Path] = []
    for path in git_files():
        try:
            if not path.is_file() or path.stat().st_size > 2 * 1024 * 1024:
                continue
            path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        files.append(path)
    return files


def check_required_files(errors: list[str]) -> None:
    required = (
        "AGENTS.md",
        "AGENTS.zh_CN.md",
        "CLAUDE.md",
        "CLAUDE.zh_CN.md",
        "docs/CHANGELOG.md",
        "dependencies.lock",
        "sdkconfig.defaults",
        "partitions.csv",
    )
    for name in required:
        if not (ROOT / name).is_file():
            errors.append(f"missing required file: {name}")

    ignored = subprocess.run(
        ["git", "check-ignore", "-q", "dependencies.lock"], cwd=ROOT
    )
    if ignored.returncode == 0:
        errors.append("dependencies.lock must be tracked, not ignored")

    for path in sorted(ROOT.glob("*.md")):
        if path.name not in ROOT_MARKDOWN_ALLOWLIST:
            errors.append(f"{path.name}: root Markdown must move to docs/")


def check_markdown_links(
    files: list[Path], errors: list[str], vendored_roots: tuple[Path, ...] = ()
) -> None:
    for path in files:
        if path.suffix.lower() != ".md" or is_vendored_document(path, vendored_roots):
            continue
        text = path.read_text(encoding="utf-8")
        for raw_target in MARKDOWN_LINK_RE.findall(text):
            target = raw_target.strip().split(maxsplit=1)[0].strip("<>")
            if not target or target.startswith(("#", "http://", "https://", "mailto:")):
                continue
            local = unquote(target.split("#", 1)[0])
            resolved = (ROOT / local.lstrip("/")) if local.startswith("/") else (path.parent / local)
            if local and not resolved.resolve().exists():
                errors.append(f"{path.relative_to(ROOT).as_posix()}: missing link target {target}")


def check_document_languages(
    files: list[Path], errors: list[str], vendored_roots: tuple[Path, ...] = ()
) -> None:
    """Require an English default and a linked Simplified Chinese peer."""
    markdown = {
        path for path in files
        if path.suffix.lower() == ".md" and not is_vendored_document(path, vendored_roots)
    }

    for path in sorted(markdown):
        name = path.name
        text = path.read_text(encoding="utf-8")
        opening = "\n".join(text.splitlines()[:8])

        if name.endswith(".zh_CN.md"):
            default_name = f"{name[:-len('.zh_CN.md')]}.md"
            default_path = path.with_name(default_name)
            if default_path not in markdown:
                errors.append(
                    f"{path.relative_to(ROOT).as_posix()}: missing English default {default_name}"
                )
            elif default_name not in opening:
                errors.append(
                    f"{path.relative_to(ROOT).as_posix()}: missing top language link to {default_name}"
                )
            continue

        chinese_name = f"{path.stem}.zh_CN.md"
        chinese_path = path.with_name(chinese_name)
        if chinese_path not in markdown:
            errors.append(
                f"{path.relative_to(ROOT).as_posix()}: missing Simplified Chinese peer {chinese_name}"
            )
        elif chinese_name not in opening:
            errors.append(
                f"{path.relative_to(ROOT).as_posix()}: missing top language link to {chinese_name}"
            )

        english_prose = text.replace("简体中文", "")
        match = CJK_RE.search(english_prose)
        if match:
            line = english_prose.count("\n", 0, match.start()) + 1
            errors.append(
                f"{path.relative_to(ROOT).as_posix()}:{line}: default Markdown must use English prose"
            )


def check_sensitive_content(files: list[Path], errors: list[str]) -> None:
    for path in files:
        text = path.read_text(encoding="utf-8")
        for label, pattern in SECRET_PATTERNS.items():
            if pattern.search(text):
                errors.append(f"{path.relative_to(ROOT).as_posix()}: possible {label}")

        # 设备二维码链接:?s=<序列号>&k=<密钥>。旧实现只看 s=(序列号)、从不看
        # k=(真正的密钥),于是"s 脱敏、k 明文"的链接会带着 PASS 进仓库。
        # 现在:任一参数不是占位符就报错,参数顺序与额外参数都不影响判定。
        for match in QR_LINK_RE.finditer(text):
            query = match.group(1)
            for key, value in re.findall(r"([A-Za-z_]+)=([^&\s)\"'`]*)", query):
                if key in ("s", "k") and value and not looks_masked(value):
                    errors.append(
                        f"{path.relative_to(ROOT).as_posix()}: possible unsanitized "
                        f"device QR link ({key}=)"
                    )
                    break


def check_conflict_markers(files: list[Path], errors: list[str]) -> None:
    # 只认"真正的冲突标记":起始行 `<<<<<<< ` / 结束行 `>>>>>>> `(7 个字符 + 空格,
    # 与 git 一致)。旧实现把单独一行 `=======` 也算冲突,而那是合法的 Markdown
    # setext 标题下划线 —— 一份正常文档会被判为"未解决的冲突"。
    # 中间那行 `=======` 不单独判定:真冲突必然同时有起始与结束标记。
    start = re.compile(r"(?m)^<<<<<<< ")
    end = re.compile(r"(?m)^>>>>>>> ")
    for path in files:
        text = path.read_text(encoding="utf-8")
        if start.search(text) or end.search(text):
            errors.append(f"{path.relative_to(ROOT).as_posix()}: unresolved merge conflict marker")


def main() -> int:
    errors: list[str] = []
    vendored_roots = vendored_document_roots(errors)
    files = text_files()
    check_required_files(errors)
    check_markdown_links(files, errors, vendored_roots)
    check_document_languages(files, errors, vendored_roots)
    check_sensitive_content(files, errors)
    check_conflict_markers(files, errors)

    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1

    print(f"Repository checks: PASS ({len(files)} text files scanned)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
