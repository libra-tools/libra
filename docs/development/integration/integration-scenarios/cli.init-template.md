### `cli.init-template`

目的：覆盖 `--template <template-directory>`。

最小步骤：

```bash
# Short form after prelude.
SCENARIO="cli.init-template"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

mkdir -p template/info template/hooks template/custom
printf 'ignored-by-template\n' > template/info/exclude
printf '#!/bin/sh\nexit 0\n' > template/hooks/pre-commit.sh
printf 'sentinel\n' > template/custom/sentinel.txt

libra init --template template templated-repo
test -f templated-repo/.libra/info/exclude
test -f templated-repo/.libra/hooks/pre-commit.sh
test -f templated-repo/.libra/custom/sentinel.txt
cd templated-repo
libra status
```

负向步骤：

```bash
cd "$RUN_DIR"
! libra init --template missing-template bad-template-repo
```

断言：模板目录内容被复制到目标仓库的 Libra 存储根；模板不会阻止 `objects/pack`、`objects/info`、`libra.db` 等必要布局创建；不存在或非目录 template 路径必须失败并在错误中标明路径。

补充可执行断言：
- 模板中的文件（exclude、pre-commit.sh、sentinel.txt）必须出现在 `templated-repo/.libra/` 对应位置。
- `libra --json init --template template templated-json` 成功后验证 `ok:true`。
- 缺失 template 目录时错误必须非 0，stderr 包含路径。
- 转换后的仓库 `libra fsck --connectivity-only` 通过。

