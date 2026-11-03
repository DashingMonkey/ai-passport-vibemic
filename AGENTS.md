# Repository Guidelines for AI Agents (Passport VibeMic)

本文件是 `passport-vibemic` 仓库根目录的入口,只做路由。本仓库包含两个互相独立的子项目,
**各自的子树内还有自己的 `AGENTS.md`,那份才是该子树的权威指令**;本文件不复述也不覆盖它们。

## 子项目与权威指令

| 子树 | 内容 | 进入后先读 |
| --- | --- | --- |
| [`device/`](device/) | 手持设备固件(FoloToy AI Passport fork,ESP-IDF 5.5.3 / ESP32-C3) | [`device/AGENTS.md`](device/AGENTS.md) |
| [`desktop/`](desktop/) | PC 端桌面软件(Rust + Tauri 2 + Vue 3) | [`desktop/AGENTS.md`](desktop/AGENTS.md)(产品说明见 [`desktop/README.md`](desktop/README.md)) |

交叉边界(重要):

- **device 的规则不适用于 desktop。** [`device/AGENTS.md`](device/AGENTS.md) 里的
  ESP-IDF 构建、烧录、LVGL、分区表、硬件事实优先级,以及 `skills/` 下那五个
  `passport-*` skill,**只对 `device/` 子树有效**。在 `desktop/` 下工作时不要加载它们,
  也不要把「驱动板子」「烧录」当成 desktop 任务的默认动作。
- **device 是上游仓库的 fork,有自己的校验闸门。** 该子树的验证命令必须在
  `device/` 目录内执行(如 `cd device && ./tools/validate.sh --static`),
  仓库根目录下没有 `tools/`,也没有 `validate.sh`。
- **不要为了「让 AI 看得见」而移动 device 的治理文件。** `device/tools/check_repo.py`
  把 `AGENTS.md`、`AGENTS.zh_CN.md`、`CLAUDE.md`、`CLAUDE.zh_CN.md` 作为
  device 仓库根的必存在文件校验,并拒绝额外的根级 Markdown;移动或新增会导致
  device 的静态校验失败。device 侧的 fork 同步与文档分区约定见
  [`device/docs/fork-guide.zh_CN.md`](device/docs/fork-guide.zh_CN.md)。

## 仓库根

根目录只保留仓库级文档:[`README.md`](README.md)(产品定位与目录导览)、
[`AGENTS.md`](AGENTS.md)(本文件)、[`LICENSE`](LICENSE) 与 `.gitignore`。
子项目自己的文档留在各自子树内;不要在根目录堆放子项目文档,
也不要新建第三份规则副本。

`device/` 目录沿用上游 MIT 许可(见 [`device/LICENSE`](device/LICENSE));
仓库其余部分见根 [`LICENSE`](LICENSE)。

## 提交与安全(两个子树共同适用)

- 先跑 `git status --short --branch`,保留用户已有改动与未跟踪文件,不要顺手清理无关文件。
- 只有用户明确要求,或当前工作流明确要求时才提交、推送、打标签。
- 任何情况下都不要提交凭据、设备二维码密钥、私钥、个人信息或未脱敏日志。
- 交付时如实区分「已验证」与「未验证」:构建通过不等于硬件验证通过。
