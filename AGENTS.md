# AGENTS.md

## 项目发布说明

本项目使用 GitHub Actions 自动发布版本。发布动作由 Git tag 触发，不是普通 commit 触发。

## 发布前检查

发布前先确认本地测试通过：

```bash
cargo fmt --check
cargo test
```

如果改过 `.github/workflows/release.yml`，也要检查 workflow：

```bash
actionlint .github/workflows/release.yml
```

## 版本号规则

发布 `vX.Y.Z` 前，先把 `Cargo.toml` 里的版本改成 `X.Y.Z`，并同步更新 `Cargo.lock`。

例如发布 `v0.0.1` 时：

```toml
version = "0.0.1"
```

## 发布步骤

1. 提交所有要发布的代码。
2. 给这个提交打 tag：

```bash
git tag v0.0.1
```

3. 先推送代码，再推送 tag：

```bash
git push origin main
git push origin v0.0.1
```

推送 tag 后，GitHub 会自动运行 `.github/workflows/release.yml`。

## 发布产物

每次发布会生成 6 个二进制包：

- `ccs-proxy-linux-amd64.tar.gz`
- `ccs-proxy-linux-arm64.tar.gz`
- `ccs-proxy-macos-amd64.tar.gz`
- `ccs-proxy-macos-arm64.tar.gz`
- `ccs-proxy-windows-amd64.zip`
- `ccs-proxy-windows-arm64.zip`

这些文件会上传到 GitHub Releases 的对应 tag 页面。

## 注意事项

- tag 必须打在包含 `.github/workflows/release.yml` 的 commit 上。
- 如果同一个 tag 的 workflow 重跑，Release 里的同名文件会被覆盖。
- 不要在未确认测试结果时发布 tag。
- 如果 tag 打错了，不要随意删除远端 tag；先确认是否已经生成 Release，再决定清理方式。
